//! `keylight products key-types ...` — the license key types on an app.
//!
//! Nested under `products` because the endpoints are, so the URL structure
//! and the command structure teach each other. The app is named with
//! `--product` rather than inferred from config: inferring it would make the
//! same command mean different things on different machines, which is exactly
//! wrong for a tool expected to be driven by agents.

use crate::commands::{Context, Deps, json_body, run_gated};
use crate::error::{CliError, Result};
use crate::output::{self, emit};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum KeyTypesCommand {
    /// List an app's key types.
    List {
        #[arg(long)]
        product: String,
    },
    /// Create a key type.
    Create {
        #[arg(long)]
        product: String,
        /// Stable id, e.g. `pro`. Server-generated when omitted.
        #[arg(long)]
        key_type_id: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
        /// How many devices one key may activate.
        #[arg(long)]
        activation_limit: Option<u32>,
        /// Days a key stays valid. Pass 0 for a perpetual key.
        #[arg(long)]
        duration_days: Option<u32>,
        /// Entitlement to grant; repeat for several.
        #[arg(long = "entitlement")]
        entitlements: Option<Vec<String>>,
    },
    /// Change a key type.
    Update {
        /// The key type to change.
        key_type_id: String,
        #[arg(long)]
        product: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        activation_limit: Option<u32>,
        /// Days a key stays valid. Pass 0 for a perpetual key.
        #[arg(long)]
        duration_days: Option<u32>,
        #[arg(long = "entitlement")]
        entitlements: Option<Vec<String>>,
    },
    /// Delete a key type. Needs approval in your browser.
    Delete {
        key_type_id: String,
        #[arg(long)]
        product: String,
    },
    /// Check that every mapped payment-provider price still resolves.
    VerifyPrices {
        #[arg(long)]
        product: String,
    },
}

/// `--duration-days 0` means perpetual, and the API spells that `null`, not
/// `0`. Sending `0` would create a key type that expires the instant it is
/// issued — the difference between "never expires" and "already expired".
fn duration_value(days: Option<u32>) -> Option<serde_json::Value> {
    match days {
        None => None,
        Some(0) => Some(serde_json::Value::Null),
        Some(d) => Some(serde_json::json!(d)),
    }
}

pub fn run(ctx: &Context, deps: &Deps, cmd: KeyTypesCommand) -> Result<()> {
    match cmd {
        KeyTypesCommand::List { product } => list(ctx, deps, &product),
        KeyTypesCommand::Create {
            product,
            key_type_id,
            display_name,
            activation_limit,
            duration_days,
            entitlements,
        } => {
            let body = json_body(vec![
                ("keyTypeId", key_type_id.map(Into::into)),
                ("displayName", display_name.map(Into::into)),
                ("activationLimit", activation_limit.map(Into::into)),
                ("durationDays", duration_value(duration_days)),
                ("entitlements", entitlements.map(Into::into)),
            ]);
            write(
                ctx,
                deps,
                "POST",
                &format!("/v1/products/{product}/key-types"),
                Some(body),
            )
        }
        KeyTypesCommand::Update {
            key_type_id,
            product,
            display_name,
            activation_limit,
            duration_days,
            entitlements,
        } => {
            let body = json_body(vec![
                ("displayName", display_name.map(Into::into)),
                ("activationLimit", activation_limit.map(Into::into)),
                ("durationDays", duration_value(duration_days)),
                ("entitlements", entitlements.map(Into::into)),
            ]);
            if body.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                return Err(CliError::Config(
                    "nothing to update; pass at least one of --display-name, \
                     --activation-limit, --duration-days, --entitlement"
                        .into(),
                ));
            }
            write(
                ctx,
                deps,
                "PATCH",
                &format!("/v1/products/{product}/key-types/{key_type_id}"),
                Some(body),
            )
        }
        KeyTypesCommand::Delete {
            key_type_id,
            product,
        } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "DELETE",
                &format!("/v1/products/{product}/key-types/{key_type_id}"),
                None,
            )?;
            emit(ctx.json, &value, |_| {
                println!("Deleted key type {key_type_id} from {product}.");
            });
            Ok(())
        }
        KeyTypesCommand::VerifyPrices { product } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/products/{product}/key-types/verify-prices"),
                None,
            )?;
            emit(ctx.json, &value, |v| {
                let results = v.get("results").and_then(|r| r.as_array());
                match results {
                    Some(items) if !items.is_empty() => {
                        let rows: Vec<Vec<String>> = items
                            .iter()
                            .map(|r| {
                                vec![
                                    output::cell(r.get("keyTypeId")),
                                    output::cell(r.get("stripePriceId")),
                                    output::cell(r.get("verified").or(r.get("ok"))),
                                ]
                            })
                            .collect();
                        output::table(&["KEY TYPE", "PRICE", "VERIFIED"], &rows);
                    }
                    _ => output::empty("No mapped prices to verify."),
                }
            });
            Ok(())
        }
    }
}

fn list(ctx: &Context, deps: &Deps, product: &str) -> Result<()> {
    let client = ctx.client(deps)?;
    let value = run_gated(
        ctx,
        deps,
        &client,
        "GET",
        &format!("/v1/products/{product}/key-types"),
        None,
    )?;
    emit(ctx.json, &value, |v| {
        // The route answers either a bare array or `{keyTypes: [...]}`.
        let items = v
            .as_array()
            .or_else(|| v.get("keyTypes").and_then(|k| k.as_array()))
            .or_else(|| v.get("items").and_then(|k| k.as_array()));
        match items {
            Some(items) if !items.is_empty() => {
                let rows: Vec<Vec<String>> = items
                    .iter()
                    .map(|k| {
                        vec![
                            output::cell(k.get("keyTypeId")),
                            output::cell(k.get("displayName")),
                            output::cell(k.get("activationLimit")),
                            match k.get("durationDays") {
                                Some(serde_json::Value::Null) | None => "perpetual".to_string(),
                                Some(d) => format!("{d} days"),
                            },
                            output::cell(k.get("entitlements")),
                        ]
                    })
                    .collect();
                output::table(
                    &["ID", "NAME", "DEVICES", "DURATION", "ENTITLEMENTS"],
                    &rows,
                );
            }
            _ => output::empty(&format!(
                "No key types on {product}. Create one with: \
                 keylight products key-types create --product {product} --key-type-id <id>"
            )),
        }
    });
    Ok(())
}

fn write(
    ctx: &Context,
    deps: &Deps,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let value = run_gated(ctx, deps, &client, method, path, body.as_ref())?;
    // Mutations print the resulting object rather than a bare success line, so
    // the human sees the new state and an agent avoids a second call.
    emit(ctx.json, &value, |v| {
        let k = v.get("keyType").unwrap_or(v);
        output::kv(&[
            ("ID", output::cell(k.get("keyTypeId"))),
            ("Name", output::cell(k.get("displayName"))),
            ("Devices", output::cell(k.get("activationLimit"))),
            (
                "Duration",
                match k.get("durationDays") {
                    Some(serde_json::Value::Null) | None => "perpetual".to_string(),
                    Some(d) => format!("{d} days"),
                },
            ),
            ("Entitlements", output::cell(k.get("entitlements"))),
        ]);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the mapping: 0 is "perpetual", which the API spells
    /// `null`. Sending `0` would mint a key type that expires immediately.
    #[test]
    fn zero_duration_days_becomes_null_not_zero() {
        assert_eq!(duration_value(Some(0)), Some(serde_json::Value::Null));
    }

    #[test]
    fn a_real_duration_passes_through() {
        assert_eq!(duration_value(Some(365)), Some(serde_json::json!(365)));
    }

    /// Unsupplied stays unsupplied — on PATCH, sending `null` would clear a
    /// duration the user never mentioned.
    #[test]
    fn an_unsupplied_duration_is_absent() {
        assert_eq!(duration_value(None), None);
    }

    // ---- wire calls ------------------------------------------------------

    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::resp;

    fn run_cmd(t: &std::sync::Arc<crate::test_support::ScriptedTransport>, cmd: KeyTypesCommand) {
        let o = RecordingOpener::new();
        let d = deps(t, &o);
        let c = ctx(true);
        run(&c, &d, cmd).unwrap();
        cleanup(&c);
    }

    #[test]
    fn create_nests_under_the_product_and_maps_perpetual_to_null() {
        let t = transport(vec![resp(201, r#"{"keyTypeId":"pro"}"#, None)]);
        run_cmd(
            &t,
            KeyTypesCommand::Create {
                product: "my-app".into(),
                key_type_id: Some("pro".into()),
                display_name: Some("Pro".into()),
                activation_limit: Some(3),
                duration_days: Some(0),
                entitlements: Some(vec!["sync".into()]),
            },
        );

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/products/my-app/key-types"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["keyTypeId"], "pro");
        assert_eq!(sent["activationLimit"], 3);
        assert_eq!(
            sent["durationDays"],
            serde_json::Value::Null,
            "0 days means perpetual, which the API spells null — sending 0 \
             would mint a key type that expires the instant it is issued"
        );
        assert_eq!(sent["entitlements"], serde_json::json!(["sync"]));
    }

    #[test]
    fn update_targets_the_key_type_under_its_product() {
        let t = transport(vec![resp(200, r#"{"keyTypeId":"pro"}"#, None)]);
        run_cmd(
            &t,
            KeyTypesCommand::Update {
                key_type_id: "pro".into(),
                product: "my-app".into(),
                display_name: Some("Pro plan".into()),
                activation_limit: None,
                duration_days: None,
                entitlements: None,
            },
        );

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "PATCH");
        assert!(
            url.ends_with("/v1/products/my-app/key-types/pro"),
            "got {url}"
        );
        assert_eq!(body.unwrap(), r#"{"displayName":"Pro plan"}"#);
    }

    /// A PATCH with nothing set would be a no-op round trip that still reads
    /// as success. Refuse it locally instead.
    #[test]
    fn an_empty_update_is_rejected_without_calling_the_api() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let err = run(
            &c,
            &d,
            KeyTypesCommand::Update {
                key_type_id: "pro".into(),
                product: "my-app".into(),
                display_name: None,
                activation_limit: None,
                duration_days: None,
                entitlements: None,
            },
        )
        .unwrap_err();
        cleanup(&c);
        assert!(err.to_string().contains("nothing to update"), "got: {err}");
        assert!(t.requests.lock().unwrap().is_empty());
    }

    /// Delete is confirm-gated server-side: 202 → poll → retry carrying the
    /// confirmation id. The command must walk that without the caller
    /// knowing, and the retried leg must reach the same URL.
    #[test]
    fn delete_walks_the_confirmation_flow_and_retries_the_same_path() {
        let pending = r#"{"confirmation":{"id":"conf1","status":"pending","approveUrl":"https://app.keylight.dev/c/conf1","expiresInSeconds":600}}"#;
        let t = transport(vec![
            resp(202, pending, None),
            resp(200, r#"{"status":"approved"}"#, Some(0)),
            resp(200, r#"{"deleted":true}"#, None),
        ]);
        run_cmd(
            &t,
            KeyTypesCommand::Delete {
                key_type_id: "pro".into(),
                product: "my-app".into(),
            },
        );

        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3, "initial call, one poll, then the retry");
        assert_eq!(reqs[0].0, "DELETE");
        assert!(
            reqs[1].1.contains("/v1/confirmations/conf1"),
            "got {}",
            reqs[1].1
        );
        assert_eq!(reqs[2].0, "DELETE");
        assert!(
            reqs[2].1.ends_with("/v1/products/my-app/key-types/pro"),
            "the retry must hit the original path: {}",
            reqs[2].1
        );
        assert!(
            reqs[2]
                .2
                .iter()
                .any(|(k, v)| k == crate::api::CONFIRMATION_HEADER && v == "conf1"),
            "the retry must carry the confirmation id"
        );
    }

    #[test]
    fn verify_prices_posts_to_the_collection_not_an_item() {
        let t = transport(vec![resp(200, r#"{"results":[]}"#, None)]);
        run_cmd(
            &t,
            KeyTypesCommand::VerifyPrices {
                product: "my-app".into(),
            },
        );

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(
            url.ends_with("/v1/products/my-app/key-types/verify-prices"),
            "got {url}"
        );
    }
}
