//! `keylight licenses ...` — the licenses themselves.
//!
//! The largest group, and the first to exercise both confirm-gated paths:
//! `revoke` returns JSON, `export` returns `text/csv`.
//!
//! Lists and detail views carry only `displayKey`, the masked form. There is
//! exactly one exception: `remint` answers with `licenseKey`, the plaintext
//! replacement — **returned once**, never again. `remint` therefore prints it,
//! because a caller who doesn't capture it there has lost it permanently.

use super::{Context, Deps, json_body, query_path, run_gated, run_gated_raw};
use crate::api::ApiOutcome;
use crate::error::{CliError, Result};
use crate::output::{self, emit};
use clap::Subcommand;

/// Header the mint/import routes accept (`IDEMPOTENCY_HEADER` in
/// `worker/src/routes/v1/licenses.ts`).
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

#[derive(Subcommand)]
pub enum LicensesCommand {
    /// List licenses.
    List {
        /// Only licenses for this app.
        #[arg(long)]
        product: Option<String>,
        /// Only licenses in this status, e.g. `active`, `revoked`.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one license.
    Get { license_id: String },
    /// Issue a license.
    Create {
        #[arg(long)]
        product: String,
        #[arg(long)]
        key_type: Option<String>,
        #[arg(long)]
        customer_email: Option<String>,
        #[arg(long)]
        customer_name: Option<String>,
        /// Email the key to the customer.
        #[arg(long)]
        send_email: bool,
        /// Reuse across retries so a repeated call cannot issue a second
        /// license. Generated per invocation when omitted.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Revoke a license. Needs approval in your browser.
    Revoke { license_id: String },
    /// Issue a replacement key, retiring the old one.
    Remint {
        license_id: String,
        #[arg(long)]
        send_email: bool,
    },
    /// Free a seat by deactivating one device.
    DeactivateDevice {
        license_id: String,
        /// The device's instance id, as shown by `licenses get`.
        #[arg(long)]
        instance_id: String,
    },
    /// Bulk-import existing licenses from a JSON file of rows.
    Import {
        #[arg(long)]
        product: String,
        /// Path to a JSON file: either an array of rows, or an object with a
        /// `rows` array.
        #[arg(long)]
        file: String,
        /// Where these licenses came from, e.g. `gumroad`.
        #[arg(long)]
        migration_source: Option<String>,
        #[arg(long)]
        send_email: bool,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Export every license as CSV. Needs approval in your browser.
    Export {
        /// Write to this file instead of stdout.
        #[arg(long)]
        output: Option<String>,
    },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: LicensesCommand) -> Result<()> {
    match cmd {
        LicensesCommand::List {
            product,
            status,
            limit,
            cursor,
        } => list(ctx, deps, product, status, limit, cursor),
        LicensesCommand::Get { license_id } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "GET",
                &format!("/v1/licenses/{license_id}"),
                None,
            )?;
            emit(ctx.json, &value, print_license);
            Ok(())
        }
        LicensesCommand::Create {
            product,
            key_type,
            customer_email,
            customer_name,
            send_email,
            idempotency_key,
        } => {
            let body = json_body(vec![
                ("productId", Some(product.into())),
                ("keyTypeId", key_type.map(Into::into)),
                ("customerEmail", customer_email.map(Into::into)),
                ("customerName", customer_name.map(Into::into)),
                ("sendEmail", send_email.then_some(true.into())),
            ]);
            mint(ctx, deps, "/v1/licenses", &body, idempotency_key)
        }
        LicensesCommand::Revoke { license_id } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/licenses/{license_id}/revoke"),
                None,
            )?;
            emit(ctx.json, &value, |v| {
                println!("Revoked. Devices lose access at their next check-in.");
                println!();
                print_license(v);
            });
            Ok(())
        }
        LicensesCommand::Remint {
            license_id,
            send_email,
        } => {
            let client = ctx.client(deps)?;
            let body = json_body(vec![("sendEmail", send_email.then_some(true.into()))]);
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/licenses/{license_id}/remint"),
                Some(&body),
            )?;
            emit(ctx.json, &value, |v| {
                // The one place a plaintext key appears. The spec is explicit
                // that it is "returned once" — not printing it here would
                // lose the new key permanently, which defeats the command.
                if let Some(key) = v.get("licenseKey").and_then(|k| k.as_str()) {
                    println!("New key: {key}");
                    println!("This is the only time it is shown. Save it now.");
                    println!();
                }
                print_license(v);
            });
            Ok(())
        }
        LicensesCommand::DeactivateDevice {
            license_id,
            instance_id,
        } => {
            let client = ctx.client(deps)?;
            let body = serde_json::json!({ "instanceId": instance_id });
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/licenses/{license_id}/deactivate-device"),
                Some(&body),
            )?;
            emit(ctx.json, &value, |v| {
                println!("Device removed. The seat is free.");
                println!();
                print_license(v);
            });
            Ok(())
        }
        LicensesCommand::Import {
            product,
            file,
            migration_source,
            send_email,
            idempotency_key,
        } => {
            let rows = read_rows(&file)?;
            let body = json_body(vec![
                ("productId", Some(product.into())),
                ("rows", Some(rows)),
                ("migrationSource", migration_source.map(Into::into)),
                ("sendEmail", send_email.then_some(true.into())),
            ]);
            mint(ctx, deps, "/v1/licenses/import", &body, idempotency_key)
        }
        LicensesCommand::Export { output } => export(ctx, deps, output),
    }
}

fn list(
    ctx: &Context,
    deps: &Deps,
    product: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let path = query_path(
        "/v1/licenses",
        &[
            ("product", product),
            ("status", status),
            ("limit", limit.map(|l| l.to_string())),
            ("cursor", cursor),
        ],
    );
    let value = run_gated(ctx, deps, &client, "GET", &path, None)?;
    emit(ctx.json, &value, |v| {
        match v.get("items").and_then(|i| i.as_array()) {
            Some(items) if !items.is_empty() => {
                let rows: Vec<Vec<String>> = items
                    .iter()
                    .map(|l| {
                        vec![
                            output::cell(l.get("displayKey")),
                            output::cell(l.get("productId")),
                            output::cell(l.get("keyTypeId")),
                            output::cell(l.get("status")),
                            format!(
                                "{}/{}",
                                output::cell(l.get("activationCount")),
                                output::cell(l.get("activationLimit"))
                            ),
                            output::date_from_ms(l.get("expiresAt")),
                        ]
                    })
                    .collect();
                output::table(
                    &["KEY", "APP", "TYPE", "STATUS", "DEVICES", "EXPIRES"],
                    &rows,
                );
                output::next_cursor_hint(v, "keylight licenses list");
            }
            _ => output::empty(
                "No licenses match. Issue one with: keylight licenses create \
                 --product <id> --customer-email <email>",
            ),
        }
    });
    Ok(())
}

/// The two routes that create licenses, both idempotency-key aware.
///
/// Neither is confirm-gated, so they go straight to the client rather than
/// through `run_gated` — `run_gated` has no way to carry the extra header.
fn mint(
    ctx: &Context,
    deps: &Deps,
    path: &str,
    body: &serde_json::Value,
    idempotency_key: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    // Always send one. A retry after a timeout is exactly when a duplicate
    // mint happens, and the caller cannot know whether the first attempt
    // reached the server.
    let key = idempotency_key.unwrap_or_else(generated_idempotency_key);
    let headers = vec![(IDEMPOTENCY_HEADER.to_string(), key)];
    let value = match client.call_with_headers("POST", path, Some(body), None, &headers)? {
        ApiOutcome::Ok { value, .. } => value,
        // Neither mint route is confirm-gated; a 202 here means the server
        // is not the one this CLI was built against.
        ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
    };
    emit(ctx.json, &value, |v| {
        if v.get("imported").is_some() || v.get("results").is_some() {
            output::kv(&[
                ("Imported", output::cell(v.get("imported"))),
                ("Skipped", output::cell(v.get("skipped"))),
                ("Failed", output::cell(v.get("failed"))),
            ]);
        } else {
            print_license(v);
        }
    });
    Ok(())
}

/// A per-invocation idempotency key.
///
/// Process id plus the wall clock is enough: the key only has to be unique
/// against *this* caller's other attempts within the server's replay window,
/// and two invocations cannot share both. Deliberately not a UUID — that
/// would be a dependency for one string.
fn generated_idempotency_key() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("cli-{}-{nanos}", std::process::id())
}

/// Accept either a bare array of rows or `{"rows": [...]}` — both are shapes
/// people actually have on disk, and rejecting one would be arbitrary.
fn read_rows(path: &str) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::Config(format!("could not read {path}: {e}")))?;
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CliError::Config(format!("{path} is not valid JSON: {e}")))?;
    let rows = match parsed {
        serde_json::Value::Array(_) => parsed,
        serde_json::Value::Object(ref o) => o
            .get("rows")
            .cloned()
            .ok_or_else(|| CliError::Config(format!("{path} has no `rows` array")))?,
        _ => {
            return Err(CliError::Config(format!(
                "{path} must be a JSON array of rows, or an object with a `rows` array"
            )));
        }
    };
    if !rows.is_array() {
        return Err(CliError::Config(format!(
            "`rows` in {path} is not an array"
        )));
    }
    Ok(rows)
}

fn export(ctx: &Context, deps: &Deps, output_path: Option<String>) -> Result<()> {
    let client = ctx.client(deps)?;
    let csv = run_gated_raw(ctx, deps, &client, "GET", "/v1/licenses/export", None).map_err(
        |e| match e {
            // The route is plan-gated on top of being confirm-gated. The
            // server's own message is fine, but the bare "HTTP 402" that the
            // generic formatter produces is not obviously about a plan.
            CliError::Api {
                status: 402,
                message,
            } => CliError::Config(format!("{message} (CSV export is an Enterprise feature)")),
            other => other,
        },
    )?;

    match output_path {
        Some(path) => {
            std::fs::write(&path, &csv)
                .map_err(|e| CliError::Config(format!("could not write {path}: {e}")))?;
            // The notice goes to stderr even here, so the two modes behave
            // identically and nothing pollutes a redirect.
            eprintln!("Wrote {path}.");
        }
        // Stdout by default is what makes it composable
        // (`keylight licenses export | jq -R`), and it is why the approval
        // notice is on stderr — the data and the progress stay on separate
        // streams. `print!` rather than `println!`: the server's CSV already
        // ends with a newline, and adding one corrupts a byte-exact diff.
        None => print!("{csv}"),
    }
    Ok(())
}

/// Renders either a bare license object or one wrapped as `{license: {...}}`
/// — `revoke`, `remint` and `deactivate-device` all answer with the wrapper
/// alongside a status flag.
fn print_license(v: &serde_json::Value) {
    let l = v.get("license").unwrap_or(v);
    output::kv(&[
        ("Key", output::cell(l.get("displayKey"))),
        ("ID", output::cell(l.get("id"))),
        ("App", output::cell(l.get("productId"))),
        ("Type", output::cell(l.get("keyTypeId"))),
        ("Status", output::cell(l.get("status"))),
        (
            "Devices",
            format!(
                "{}/{}",
                output::cell(l.get("activationCount")),
                output::cell(l.get("activationLimit"))
            ),
        ),
        ("Expires", output::date_from_ms(l.get("expiresAt"))),
        ("Created", output::date_from_ms(l.get("createdAt"))),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::Arc;

    const LICENSE: &str = r#"{"id":"abc","productId":"my-app","keyTypeId":"pro","status":"active","displayKey":"MYAP-••••-1234","activationLimit":3,"activationCount":1,"expiresAt":null,"createdAt":1781775062000,"updatedAt":1781775062000}"#;

    fn run_cmd(t: &Arc<ScriptedTransport>, cmd: LicensesCommand) -> Result<()> {
        let o = RecordingOpener::new();
        let d = deps(t, &o);
        let c = ctx(true);
        let r = run(&c, &d, cmd);
        cleanup(&c);
        r
    }

    #[test]
    fn list_sends_every_supplied_filter_as_a_query_param() {
        let t = transport(vec![resp(200, r#"{"items":[],"nextCursor":null}"#, None)]);
        run_cmd(
            &t,
            LicensesCommand::List {
                product: Some("my-app".into()),
                status: Some("active".into()),
                limit: Some(50),
                cursor: None,
            },
        )
        .unwrap();

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "GET");
        assert!(url.contains("product=my-app"), "got {url}");
        assert!(url.contains("status=active"), "got {url}");
        assert!(url.contains("limit=50"), "got {url}");
        assert!(!url.contains("cursor="), "an unset filter must be absent");
    }

    /// `productId` is the one required field; the rest must be absent when
    /// not supplied rather than sent as null.
    #[test]
    fn create_sends_product_id_and_omits_unsupplied_fields() {
        let t = transport(vec![resp(201, LICENSE, None)]);
        run_cmd(
            &t,
            LicensesCommand::Create {
                product: "my-app".into(),
                key_type: Some("pro".into()),
                customer_email: Some("a@b.com".into()),
                customer_name: None,
                send_email: false,
                idempotency_key: Some("fixed-key".into()),
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/licenses"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["productId"], "my-app");
        assert_eq!(sent["keyTypeId"], "pro");
        assert_eq!(sent["customerEmail"], "a@b.com");
        let o = sent.as_object().unwrap();
        assert!(!o.contains_key("customerName"));
        assert!(
            !o.contains_key("sendEmail"),
            "an unset --send-email must be absent, not false"
        );
    }

    /// A retried mint that the server already processed would issue a second
    /// license to the same customer, so the header goes on every attempt —
    /// including when the caller did not supply a key.
    #[test]
    fn create_always_sends_an_idempotency_key() {
        let t = transport(vec![resp(201, LICENSE, None)]);
        run_cmd(
            &t,
            LicensesCommand::Create {
                product: "my-app".into(),
                key_type: None,
                customer_email: None,
                customer_name: None,
                send_email: false,
                idempotency_key: None,
            },
        )
        .unwrap();

        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        let key = headers
            .iter()
            .find(|(k, _)| k == IDEMPOTENCY_HEADER)
            .map(|(_, v)| v.clone())
            .expect("Idempotency-Key must be sent even when not supplied");
        assert!(!key.is_empty());
    }

    #[test]
    fn an_explicit_idempotency_key_is_used_verbatim() {
        let t = transport(vec![resp(201, LICENSE, None)]);
        run_cmd(
            &t,
            LicensesCommand::Create {
                product: "my-app".into(),
                key_type: None,
                customer_email: None,
                customer_name: None,
                send_email: false,
                idempotency_key: Some("my-key-1".into()),
            },
        )
        .unwrap();

        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        assert!(headers.contains(&(IDEMPOTENCY_HEADER.to_string(), "my-key-1".to_string())));
    }

    #[test]
    fn deactivate_device_sends_the_instance_id_the_route_requires() {
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            LicensesCommand::DeactivateDevice {
                license_id: "abc".into(),
                instance_id: "inst-1".into(),
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(
            url.ends_with("/v1/licenses/abc/deactivate-device"),
            "got {url}"
        );
        assert_eq!(body.unwrap(), r#"{"instanceId":"inst-1"}"#);
    }

    /// Revoke is confirm-gated: 202 → poll → retry with the confirmation id.
    #[test]
    fn revoke_walks_the_confirmation_flow() {
        let pending = r#"{"confirmation":{"id":"conf1","status":"pending","approveUrl":"https://app.keylight.dev/c/conf1","expiresInSeconds":600}}"#;
        let t = transport(vec![
            resp(202, pending, None),
            resp(200, r#"{"status":"approved"}"#, Some(0)),
            resp(200, r#"{"revoked":true}"#, None),
        ]);
        run_cmd(
            &t,
            LicensesCommand::Revoke {
                license_id: "abc".into(),
            },
        )
        .unwrap();

        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(reqs[2].1.ends_with("/v1/licenses/abc/revoke"));
        assert!(
            reqs[2]
                .2
                .iter()
                .any(|(k, v)| k == crate::api::CONFIRMATION_HEADER && v == "conf1")
        );
    }

    /// The export answers text/csv. Before `call_raw` existed this could not
    /// have worked at all — the JSON parse rejected it as InvalidResponse.
    #[test]
    fn export_writes_the_raw_csv_to_a_file_without_parsing_it() {
        let csv = "id,status\nabc,active\n";
        let t = transport(vec![resp(200, csv, None)]);
        let path = std::env::temp_dir().join("keylight-cli-export-test.csv");
        run_cmd(
            &t,
            LicensesCommand::Export {
                output: Some(path.display().to_string()),
            },
        )
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written, csv,
            "the CSV must land byte-for-byte, not round-tripped through JSON"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Plan-gating is a distinct failure from a missing scope, and "HTTP 402"
    /// alone doesn't tell anyone what to do about it.
    #[test]
    fn export_on_a_non_enterprise_plan_explains_the_upgrade() {
        let t = transport(vec![resp(
            402,
            r#"{"error":"Upgrade to Enterprise for CSV export."}"#,
            None,
        )]);
        let err = run_cmd(&t, LicensesCommand::Export { output: None }).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Enterprise"), "got: {msg}");
    }

    #[test]
    fn import_accepts_a_bare_array_of_rows() {
        let dir = std::env::temp_dir().join("keylight-cli-import-array.json");
        std::fs::write(&dir, r#"[{"licenseKey":"K1"},{"licenseKey":"K2"}]"#).unwrap();
        let rows = read_rows(&dir.display().to_string()).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn import_also_accepts_an_object_wrapping_a_rows_array() {
        let p = std::env::temp_dir().join("keylight-cli-import-object.json");
        std::fs::write(&p, r#"{"rows":[{"licenseKey":"K1"}]}"#).unwrap();
        let rows = read_rows(&p.display().to_string()).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    /// A JSON document that is neither shape must be refused with a message
    /// that says what was expected — not sent as a malformed `rows`.
    #[test]
    fn import_rejects_a_document_that_is_neither_shape() {
        let p = std::env::temp_dir().join("keylight-cli-import-bad.json");
        std::fs::write(&p, r#"{"licenses":[]}"#).unwrap();
        let err = read_rows(&p.display().to_string()).unwrap_err();
        assert!(err.to_string().contains("rows"), "got: {err}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn import_reports_a_missing_file_by_name() {
        let err = read_rows("/definitely/not/here.json").unwrap_err();
        assert!(err.to_string().contains("/definitely/not/here.json"));
    }

    /// Two invocations must not collide, or the second would be silently
    /// treated as a replay of the first and issue nothing.
    #[test]
    fn generated_idempotency_keys_differ_between_calls() {
        assert_ne!(generated_idempotency_key(), generated_idempotency_key());
    }
}
