//! `keylight customers ...`
//!
//! A customer's id is a sha256 hash of their email, not a sequential id, so
//! `customers get` takes the hash that `customers list` prints rather than
//! an address.

use super::{Context, Deps, json_body, parse_json_flag, query_path, run_gated};
use crate::error::Result;
use crate::output::{self, emit};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum CustomersCommand {
    /// List customers.
    List {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one customer.
    Get {
        /// The customer id, as shown by `customers list`.
        customer_id: String,
    },
    /// Create a customer.
    Create {
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Set the entitlements a customer holds on one of their licenses.
    Entitlements {
        #[command(subcommand)]
        command: EntitlementsCommand,
    },
    /// Attach an existing license to this customer.
    LinkLicense {
        customer_id: String,
        #[arg(long = "license")]
        license_id: String,
    },
}

#[derive(Subcommand)]
pub enum EntitlementsCommand {
    /// Replace the entitlements on a customer's license.
    Set {
        customer_id: String,
        #[arg(long = "license")]
        license_id: String,
        /// Entitlements as JSON, e.g. '["sync","export"]'.
        #[arg(long)]
        entitlements: String,
    },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: CustomersCommand) -> Result<()> {
    match cmd {
        CustomersCommand::List { limit, cursor } => list(ctx, deps, limit, cursor),
        CustomersCommand::Get { customer_id } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "GET",
                &format!("/v1/customers/{customer_id}"),
                None,
            )?;
            emit(ctx.json, &value, print_customer);
            Ok(())
        }
        CustomersCommand::Create { email, name } => {
            let client = ctx.client(deps)?;
            let body = json_body(vec![
                ("email", Some(email.into())),
                ("name", name.map(Into::into)),
            ]);
            let value = run_gated(ctx, deps, &client, "POST", "/v1/customers", Some(&body))?;
            emit(ctx.json, &value, print_customer);
            Ok(())
        }
        CustomersCommand::Entitlements { command } => match command {
            EntitlementsCommand::Set {
                customer_id,
                license_id,
                entitlements,
            } => {
                let client = ctx.client(deps)?;
                let ents = parse_json_flag(Some(entitlements), "--entitlements")?;
                let body = json_body(vec![
                    ("licenseId", Some(license_id.into())),
                    ("entitlements", ents),
                ]);
                let value = run_gated(
                    ctx,
                    deps,
                    &client,
                    "PATCH",
                    &format!("/v1/customers/{customer_id}/entitlements"),
                    Some(&body),
                )?;
                // Answers {entitlements, overridden, keyTypeDefault, license}
                // — "overridden" is the thing worth seeing: it says whether
                // this license now diverges from its key type's default.
                emit(ctx.json, &value, |v| {
                    output::kv(&[
                        ("Entitlements", output::cell(v.get("entitlements"))),
                        ("Overridden", output::cell(v.get("overridden"))),
                        ("Key type default", output::cell(v.get("keyTypeDefault"))),
                    ]);
                });
                Ok(())
            }
        },
        CustomersCommand::LinkLicense {
            customer_id,
            license_id,
        } => {
            let client = ctx.client(deps)?;
            let body = serde_json::json!({ "licenseId": license_id });
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/customers/{customer_id}/link-license"),
                Some(&body),
            )?;
            emit(ctx.json, &value, |v| {
                println!("Linked {license_id} to {customer_id}.");
                if let Some(l) = v.get("license") {
                    println!();
                    output::kv(&[
                        ("Key", output::cell(l.get("displayKey"))),
                        ("App", output::cell(l.get("productId"))),
                        ("Status", output::cell(l.get("status"))),
                    ]);
                }
            });
            Ok(())
        }
    }
}

fn list(ctx: &Context, deps: &Deps, limit: Option<u32>, cursor: Option<String>) -> Result<()> {
    let client = ctx.client(deps)?;
    let path = query_path(
        "/v1/customers",
        &[("limit", limit.map(|l| l.to_string())), ("cursor", cursor)],
    );
    let value = run_gated(ctx, deps, &client, "GET", &path, None)?;
    emit(ctx.json, &value, |v| {
        match v.get("items").and_then(|i| i.as_array()) {
            Some(items) if !items.is_empty() => {
                let rows: Vec<Vec<String>> = items
                    .iter()
                    .map(|c| {
                        vec![
                            output::cell(c.get("id")),
                            output::cell(c.get("email")),
                            output::cell(c.get("name")),
                            output::date_from_ms(c.get("createdAt")),
                        ]
                    })
                    .collect();
                output::table(&["ID", "EMAIL", "NAME", "CREATED"], &rows);
                output::next_cursor_hint(v, "keylight customers list");
            }
            _ => output::empty(
                "No customers yet. Create one with: keylight customers create --email <email>",
            ),
        }
    });
    Ok(())
}

fn print_customer(v: &serde_json::Value) {
    let c = v.get("customer").unwrap_or(v);
    output::kv(&[
        ("ID", output::cell(c.get("id"))),
        ("Email", output::cell(c.get("email"))),
        ("Name", output::cell(c.get("name"))),
        ("Created", output::date_from_ms(c.get("createdAt"))),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::Arc;

    fn run_cmd(t: &Arc<ScriptedTransport>, cmd: CustomersCommand) -> Result<()> {
        let o = RecordingOpener::new();
        let d = deps(t, &o);
        let c = ctx(true);
        let r = run(&c, &d, cmd);
        cleanup(&c);
        r
    }

    #[test]
    fn create_sends_the_required_email() {
        let t = transport(vec![resp(
            201,
            r#"{"id":"h1","email":"a@b.com","name":"A","createdAt":0}"#,
            None,
        )]);
        run_cmd(
            &t,
            CustomersCommand::Create {
                email: "a@b.com".into(),
                name: None,
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/customers"), "got {url}");
        assert_eq!(body.unwrap(), r#"{"email":"a@b.com"}"#);
    }

    /// Both `licenseId` and `entitlements` are required, and the
    /// entitlements flag carries JSON that must reach the wire as JSON
    /// rather than as a quoted string.
    #[test]
    fn entitlements_set_sends_both_required_fields_with_json_intact() {
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            CustomersCommand::Entitlements {
                command: EntitlementsCommand::Set {
                    customer_id: "h1".into(),
                    license_id: "L1".into(),
                    entitlements: r#"["sync","export"]"#.into(),
                },
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "PATCH");
        assert!(url.ends_with("/v1/customers/h1/entitlements"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["licenseId"], "L1");
        assert_eq!(sent["entitlements"], serde_json::json!(["sync", "export"]));
    }

    #[test]
    fn malformed_entitlements_json_fails_before_any_request() {
        let t = transport(vec![]);
        let err = run_cmd(
            &t,
            CustomersCommand::Entitlements {
                command: EntitlementsCommand::Set {
                    customer_id: "h1".into(),
                    license_id: "L1".into(),
                    entitlements: "[not json".into(),
                },
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--entitlements"), "got: {err}");
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn link_license_posts_the_license_id() {
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            CustomersCommand::LinkLicense {
                customer_id: "h1".into(),
                license_id: "L1".into(),
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/customers/h1/link-license"), "got {url}");
        assert_eq!(body.unwrap(), r#"{"licenseId":"L1"}"#);
    }
}
