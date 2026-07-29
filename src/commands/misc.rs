//! The four one-or-two-endpoint groups: `sdk-key`, `webhook`, `usage` and
//! `test-purchase`. Each is too small for its own file; keeping them together
//! is easier to hold in context than four near-empty modules.

use super::{Context, Deps, json_body, parse_json_flag, query_path, run_gated};
use crate::error::Result;
use crate::output::{self, emit};
use clap::Subcommand;

// ---- sdk-key ------------------------------------------------------------

#[derive(Subcommand)]
pub enum SdkKeyCommand {
    /// Print the SDK key. Needs approval in your browser.
    Get,
    /// Issue a new SDK key, invalidating the old one. Needs approval.
    Rotate,
}

pub fn run_sdk_key(ctx: &Context, deps: &Deps, cmd: SdkKeyCommand) -> Result<()> {
    let client = ctx.client(deps)?;
    let (method, path) = match cmd {
        SdkKeyCommand::Get => ("GET", "/v1/sdk-key"),
        SdkKeyCommand::Rotate => ("POST", "/v1/sdk-key/rotate"),
    };
    let value = run_gated(ctx, deps, &client, method, path, None)?;
    emit(ctx.json, &value, |v| {
        match v.get("sdkKey").and_then(|k| k.as_str()) {
            Some(key) => println!("{key}"),
            None => output::kv(&[("Result", v.to_string())]),
        }
        if method == "POST" {
            eprintln!(
                "The previous SDK key is now invalid. Ship this one before your \
                 apps next reach the API."
            );
        }
    });
    Ok(())
}

// ---- webhook ------------------------------------------------------------

#[derive(Subcommand)]
pub enum WebhookCommand {
    /// Show the configured webhook.
    Get,
    /// Set or clear the webhook. Needs approval in your browser.
    Set {
        /// Destination URL. Pass an empty string to turn delivery off.
        #[arg(long)]
        url: String,
        /// Stop Keylight emailing the customer on license events.
        #[arg(long)]
        disable_customer_email: bool,
        /// Event subscription map as JSON.
        #[arg(long)]
        events: Option<String>,
    },
}

pub fn run_webhook(ctx: &Context, deps: &Deps, cmd: WebhookCommand) -> Result<()> {
    let client = ctx.client(deps)?;
    match cmd {
        WebhookCommand::Get => {
            let value = run_gated(ctx, deps, &client, "GET", "/v1/webhook", None)?;
            // WebhookSettings is {url, hasSigningSecret, disableCustomerEmail,
            // events}. The secret itself is never returned on a read — only
            // whether one exists.
            emit(ctx.json, &value, |v| {
                output::kv(&[
                    ("URL", output::cell(v.get("url"))),
                    ("Signing secret", output::cell(v.get("hasSigningSecret"))),
                    (
                        "Customer email",
                        match v.get("disableCustomerEmail").and_then(|d| d.as_bool()) {
                            Some(true) => "disabled".into(),
                            _ => "enabled".into(),
                        },
                    ),
                    ("Events", output::cell(v.get("events"))),
                ]);
            });
            Ok(())
        }
        WebhookCommand::Set {
            url,
            disable_customer_email,
            events,
        } => {
            // The schema is `string | null`, and clearing the webhook is the
            // only way to turn delivery off — so an empty --url is a real
            // instruction, not a missing argument.
            let url_value = if url.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(url.clone())
            };
            let body = json_body(vec![
                ("url", Some(url_value)),
                (
                    "disableCustomerEmail",
                    disable_customer_email.then_some(true.into()),
                ),
                ("events", parse_json_flag(events, "--events")?),
            ]);
            let value = run_gated(ctx, deps, &client, "PUT", "/v1/webhook", Some(&body))?;
            emit(ctx.json, &value, |v| {
                if url.is_empty() {
                    println!("Webhook cleared. Nothing will be delivered.");
                    return;
                }
                println!("Webhook set to {url}.");
                // Unlike the read, the update DOES return the signing secret.
                // Not printing it would mean the caller cannot verify
                // signatures on the very deliveries they just enabled.
                if let Some(sec) = v.get("signingSecret").and_then(|s| s.as_str()) {
                    println!();
                    println!("Signing secret: {sec}");
                    println!("Use it to verify the signature on every delivery.");
                }
            });
            Ok(())
        }
    }
}

// ---- usage --------------------------------------------------------------

pub fn run_usage(ctx: &Context, deps: &Deps, days: Option<u32>) -> Result<()> {
    let client = ctx.client(deps)?;
    let path = query_path("/v1/usage", &[("days", days.map(|d| d.to_string()))]);
    let value = run_gated(ctx, deps, &client, "GET", &path, None)?;
    emit(ctx.json, &value, |v| {
        // UsageDay is {day, activations, validations, activeDevices, mau};
        // the response also carries a summary object. The field is `day`, not
        // `date`, and `activeDevices`, not `devices`.
        if let Some(sum) = v.get("summary") {
            output::kv(&[
                ("Active devices", output::cell(sum.get("activeDevices"))),
                ("Monthly active", output::cell(sum.get("mau"))),
                ("Activations (7d)", output::cell(sum.get("activations7d"))),
                ("Validations (7d)", output::cell(sum.get("validations7d"))),
            ]);
            println!();
        }
        let table_rows = usage_rows(v);
        if table_rows.is_empty() {
            output::empty("No usage recorded in this window.");
        } else {
            output::table(
                &["DAY", "ACTIVATIONS", "VALIDATIONS", "DEVICES", "MAU"],
                &table_rows,
            );
        }
    });
    Ok(())
}

/// Pulled out of the renderer so the field names can be asserted directly.
/// The whole class of bug this guards is reading a field the API does not
/// have: `output::cell` answers `-` for a missing key, so a renderer built on
/// guessed names still prints a well-formed, entirely empty table.
fn usage_rows(v: &serde_json::Value) -> Vec<Vec<String>> {
    v.get("days")
        .and_then(|d| d.as_array())
        .map(|rows| {
            rows.iter()
                .map(|d| {
                    vec![
                        output::cell(d.get("day")),
                        output::cell(d.get("activations")),
                        output::cell(d.get("validations")),
                        output::cell(d.get("activeDevices")),
                        output::cell(d.get("mau")),
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- test-purchase ------------------------------------------------------

#[derive(Subcommand)]
pub enum TestPurchaseCommand {
    /// Run a test purchase end to end.
    Create {
        #[arg(long)]
        product: String,
        #[arg(long)]
        key_type: Option<String>,
    },
    /// Show a test purchase run.
    Get { run_id: String },
}

pub fn run_test_purchase(ctx: &Context, deps: &Deps, cmd: TestPurchaseCommand) -> Result<()> {
    let client = ctx.client(deps)?;
    match cmd {
        TestPurchaseCommand::Create { product, key_type } => {
            let body = json_body(vec![
                ("productId", Some(product.into())),
                ("keyTypeId", key_type.map(Into::into)),
            ]);
            let value = run_gated(ctx, deps, &client, "POST", "/v1/test-purchase", Some(&body))?;
            emit(ctx.json, &value, print_run);
            Ok(())
        }
        TestPurchaseCommand::Get { run_id } => {
            let value = run_gated(
                ctx,
                deps,
                &client,
                "GET",
                &format!("/v1/test-purchase/{run_id}"),
                None,
            )?;
            emit(ctx.json, &value, print_run);
            Ok(())
        }
    }
}

fn print_run(v: &serde_json::Value) {
    let r = v.get("run").unwrap_or(v);
    output::kv(&[
        ("Run", output::cell(r.get("runId").or(r.get("id")))),
        ("Status", output::cell(r.get("status"))),
        ("App", output::cell(r.get("productId"))),
        ("Key", output::cell(r.get("displayKey"))),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::resp;

    /// A renderer built on guessed field names still prints a well-formed
    /// table — every cell just reads `-`. Request-shape tests cannot see
    /// that, which is how the first version of this command shipped reading
    /// `date`/`devices` when `UsageDay` actually has `day`/`activeDevices`.
    ///
    /// The body below uses the field names from `UsageDay` in the spec.
    #[test]
    fn usage_rows_read_the_field_names_the_api_actually_returns() {
        let body = serde_json::json!({
            "days": [{
                "day": "2026-07-28",
                "activations": 12,
                "validations": 340,
                "activeDevices": 9,
                "mau": 7
            }],
            "summary": {"activeDevices": 9, "mau": 7, "activations7d": 40, "validations7d": 900}
        });
        let rows = usage_rows(&body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], vec!["2026-07-28", "12", "340", "9", "7"]);
        assert!(
            !rows[0].iter().any(|c| c == "-"),
            "a `-` means the renderer read a field the API does not return: {:?}",
            rows[0]
        );
    }

    /// The inverse: the field names that were wrong must NOT resolve, so this
    /// test fails loudly if someone reintroduces them.
    #[test]
    fn the_old_guessed_usage_field_names_resolve_to_nothing() {
        let body = serde_json::json!({"days": [{"date": "2026-07-28", "devices": 9}]});
        let rows = usage_rows(&body);
        assert!(
            rows[0].iter().all(|c| c == "-"),
            "`date`/`devices` are not fields of UsageDay; if this passes with \
             real values the schema changed: {:?}",
            rows[0]
        );
    }

    /// Reading the SDK key is confirm-gated, exactly like rotating it —
    /// a read that hands over a live credential is a tier-2 operation.
    #[test]
    fn sdk_key_get_walks_the_confirmation_flow() {
        let pending = r#"{"confirmation":{"id":"c1","status":"pending","approveUrl":"https://app.keylight.dev/c/c1","expiresInSeconds":600}}"#;
        let t = transport(vec![
            resp(202, pending, None),
            resp(200, r#"{"status":"approved"}"#, Some(0)),
            resp(200, r#"{"sdkKey":"klsk_live"}"#, None),
        ]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_sdk_key(&c, &d, SdkKeyCommand::Get).unwrap();
        cleanup(&c);

        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].0, "GET");
        assert!(reqs[2].1.ends_with("/v1/sdk-key"), "got {}", reqs[2].1);
    }

    #[test]
    fn sdk_key_rotate_posts_to_the_rotate_path() {
        let t = transport(vec![resp(200, r#"{"sdkKey":"klsk_new"}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_sdk_key(&c, &d, SdkKeyCommand::Rotate).unwrap();
        cleanup(&c);

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/sdk-key/rotate"), "got {url}");
    }

    /// An empty --url is the documented way to turn delivery off, and the
    /// schema spells that `null`. Sending `""` would be a malformed URL.
    #[test]
    fn an_empty_webhook_url_is_sent_as_null_not_an_empty_string() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_webhook(
            &c,
            &d,
            WebhookCommand::Set {
                url: String::new(),
                disable_customer_email: false,
                events: None,
            },
        )
        .unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "PUT");
        assert!(url.ends_with("/v1/webhook"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["url"], serde_json::Value::Null);
        assert!(sent.as_object().unwrap().contains_key("url"));
    }

    #[test]
    fn a_real_webhook_url_is_sent_as_a_string() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_webhook(
            &c,
            &d,
            WebhookCommand::Set {
                url: "https://hooks.example/keylight".into(),
                disable_customer_email: true,
                events: None,
            },
        )
        .unwrap();
        cleanup(&c);

        let (_, _, body) = request_at(&t, 0);
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["url"], "https://hooks.example/keylight");
        assert_eq!(sent["disableCustomerEmail"], true);
    }

    #[test]
    fn usage_passes_days_as_a_query_param_and_omits_it_when_unset() {
        let t = transport(vec![resp(200, r#"{"days":[]}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_usage(&c, &d, Some(30)).unwrap();
        cleanup(&c);
        let (_, url, _) = request_at(&t, 0);
        assert!(url.contains("days=30"), "got {url}");

        let t2 = transport(vec![resp(200, r#"{"days":[]}"#, None)]);
        let o2 = RecordingOpener::new();
        let d2 = deps(&t2, &o2);
        let c2 = ctx(true);
        run_usage(&c2, &d2, None).unwrap();
        cleanup(&c2);
        let (_, url2, _) = request_at(&t2, 0);
        assert!(!url2.contains("days="), "got {url2}");
    }

    #[test]
    fn test_purchase_create_sends_the_required_product_id() {
        let t = transport(vec![resp(200, r#"{"runId":"r1","status":"queued"}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_test_purchase(
            &c,
            &d,
            TestPurchaseCommand::Create {
                product: "my-app".into(),
                key_type: None,
            },
        )
        .unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/test-purchase"), "got {url}");
        assert_eq!(body.unwrap(), r#"{"productId":"my-app"}"#);
    }

    #[test]
    fn test_purchase_get_targets_the_run() {
        let t = transport(vec![resp(200, r#"{"runId":"r1","status":"done"}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        run_test_purchase(
            &c,
            &d,
            TestPurchaseCommand::Get {
                run_id: "r1".into(),
            },
        )
        .unwrap();
        cleanup(&c);

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "GET");
        assert!(url.ends_with("/v1/test-purchase/r1"), "got {url}");
    }
}
