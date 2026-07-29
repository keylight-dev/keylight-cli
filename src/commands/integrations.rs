//! `keylight integrations ...` — payment providers.
//!
//! The one place the command surface and the URL structure diverge: Stripe
//! connects through OAuth and the API gives it its own path
//! (`/v1/integrations/stripe/connect`), while every other provider connects
//! by handing over keys at `/v1/integrations/{provider}/connect`. One
//! command covers both, and a test pins each route.

use super::{Context, Deps, json_body, query_path, run_gated, secret_from_env};
use crate::error::Result;
use crate::output::{self, emit};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum IntegrationsCommand {
    /// Show every provider and whether it is connected.
    List,
    /// Show the Stripe connection in detail.
    Status,
    /// Connect a payment provider.
    ///
    /// Stripe returns an OAuth URL to open. Other providers take their API
    /// credentials, which are read from the environment rather than flags —
    /// argv is visible in shell history and to other users via `ps`.
    Connect {
        /// Provider name, e.g. `stripe`, `paddle`, `lemonsqueezy`, `polar`.
        provider: String,
        /// Stripe only: which Stripe account to connect.
        #[arg(long, value_parser = ["live", "test"])]
        mode: Option<String>,
        /// Non-Stripe: which environment these credentials are for.
        #[arg(long, value_parser = ["sandbox", "live"])]
        environment: Option<String>,
        /// Env var holding the provider's API key.
        #[arg(long)]
        api_key_env: Option<String>,
        /// Env var holding the provider's secret.
        #[arg(long)]
        secret_env: Option<String>,
        /// Env var holding the provider's webhook signing secret.
        #[arg(long)]
        webhook_secret_env: Option<String>,
    },
    /// Disconnect a provider.
    Disconnect {
        provider: String,
        #[arg(long)]
        environment: Option<String>,
    },
    /// Rotate a provider's webhook secret. Needs approval in your browser.
    RotateSecret { provider: String },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: IntegrationsCommand) -> Result<()> {
    match cmd {
        IntegrationsCommand::List => {
            let client = ctx.client(deps)?;
            let value = run_gated(ctx, deps, &client, "GET", "/v1/integrations", None)?;
            // Answers {providers: [ProviderStatus], stripe: StripeConnectStatus}
            // — NOT a bare array, and not keyed "integrations".
            emit(ctx.json, &value, |v| {
                let rows = provider_rows(v);
                if rows.is_empty() {
                    output::empty(
                        "No providers configured. Connect one with: \
                         keylight integrations connect <provider>",
                    );
                } else {
                    output::table(&["PROVIDER", "LABEL", "CONNECTED", "PRICES"], &rows);
                }
                if let Some(st) = v.get("stripe") {
                    println!();
                    output::kv(&[
                        ("Stripe connected", output::cell(st.get("connected"))),
                        (
                            "Stripe live",
                            output::cell(st.get("live").and_then(|l| l.get("accountId"))),
                        ),
                        (
                            "Stripe sandbox",
                            output::cell(st.get("sandbox").and_then(|l| l.get("accountId"))),
                        ),
                    ]);
                }
            });
            Ok(())
        }
        IntegrationsCommand::Status => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "GET",
                "/v1/integrations/stripe/status",
                None,
            )?;
            // StripeConnectStatus is {connected, live, sandbox, products},
            // where live/sandbox are each {connected, accountId}. There is no
            // flat accountId/mode/chargesEnabled.
            emit(ctx.json, &value, |v| {
                let slot = |k: &str, f: &str| output::cell(v.get(k).and_then(|s| s.get(f)));
                output::kv(&[
                    ("Connected", output::cell(v.get("connected"))),
                    ("Live account", slot("live", "accountId")),
                    ("Live connected", slot("live", "connected")),
                    ("Sandbox account", slot("sandbox", "accountId")),
                    ("Sandbox connected", slot("sandbox", "connected")),
                    ("Apps", output::cell(v.get("products"))),
                ]);
            });
            Ok(())
        }
        IntegrationsCommand::Connect {
            provider,
            mode,
            environment,
            api_key_env,
            secret_env,
            webhook_secret_env,
        } => connect(
            ctx,
            deps,
            &provider,
            mode,
            environment,
            api_key_env,
            secret_env,
            webhook_secret_env,
        ),
        IntegrationsCommand::Disconnect {
            provider,
            environment,
        } => {
            let client = ctx.client(deps)?;
            let path = query_path(
                &format!("/v1/integrations/{provider}"),
                &[("environment", environment)],
            );
            let value = run_gated(ctx, deps, &client, "DELETE", &path, None)?;
            emit(ctx.json, &value, |v| {
                println!("Disconnected {provider}.");
                // remainingEnvironments matters: disconnecting sandbox leaves
                // live connected, and silence there reads as "all gone".
                if let Some(rest) = v.get("remainingEnvironments") {
                    output::kv(&[("Still connected", output::cell(Some(rest)))]);
                }
            });
            Ok(())
        }
        IntegrationsCommand::RotateSecret { provider } => {
            let client = ctx.client(deps)?;
            let value = run_gated(
                ctx,
                deps,
                &client,
                "POST",
                &format!("/v1/integrations/{provider}/rotate-secret"),
                None,
            )?;
            // Answers {rotated, provider, pingUrl}. The secret itself is
            // never returned — the provider is reconfigured server-side, and
            // pingUrl is how you confirm delivery still works.
            emit(ctx.json, &value, |v| {
                println!("Rotated the {provider} webhook secret.");
                if let Some(u) = v.get("pingUrl").and_then(|u| u.as_str()) {
                    println!("Verify delivery at: {u}");
                }
            });
            Ok(())
        }
    }
}

/// Pulled out of the renderer so the field names can be asserted. The
/// overview answers `{providers, stripe}` — not a bare array, and not keyed
/// `integrations`, which is what the first version of this command looked for.
fn provider_rows(v: &serde_json::Value) -> Vec<Vec<String>> {
    v.get("providers")
        .and_then(|p| p.as_array())
        .map(|items| {
            items
                .iter()
                .map(|i| {
                    vec![
                        output::cell(i.get("provider")),
                        output::cell(i.get("label")),
                        output::cell(i.get("connected")),
                        output::cell(i.get("mappedPrices")),
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn connect(
    ctx: &Context,
    deps: &Deps,
    provider: &str,
    mode: Option<String>,
    environment: Option<String>,
    api_key_env: Option<String>,
    secret_env: Option<String>,
    webhook_secret_env: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let is_stripe = provider.eq_ignore_ascii_case("stripe");

    let (path, body) = if is_stripe {
        (
            "/v1/integrations/stripe/connect".to_string(),
            json_body(vec![("mode", mode.map(Into::into))]),
        )
    } else {
        // Each credential is optional: providers differ in which they need,
        // and the server validates. Only the ones named are read.
        let api_key = api_key_env.map(|v| secret_from_env(&v)).transpose()?;
        let secret = secret_env.map(|v| secret_from_env(&v)).transpose()?;
        let webhook_secret = webhook_secret_env
            .map(|v| secret_from_env(&v))
            .transpose()?;
        (
            format!("/v1/integrations/{provider}/connect"),
            json_body(vec![
                ("apiKey", api_key.map(Into::into)),
                ("secret", secret.map(Into::into)),
                ("webhookSecret", webhook_secret.map(Into::into)),
                ("environment", environment.map(Into::into)),
            ]),
        )
    };

    let value = run_gated(ctx, deps, &client, "POST", &path, Some(&body))?;
    emit(ctx.json, &value, |v| {
        // Stripe answers with an OAuth URL the human has to visit. Same rule
        // as everywhere else: print it always, open it only at a TTY.
        match v
            .get("url")
            .or(v.get("connectUrl"))
            .and_then(|u| u.as_str())
        {
            Some(url) => {
                println!("Finish connecting {provider} at: {url}");
                if !ctx.no_browser && ctx.is_tty {
                    deps.opener.open(url);
                }
            }
            None => println!("Connected {provider}."),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::Arc;

    fn run_cmd(t: &Arc<ScriptedTransport>, cmd: IntegrationsCommand) -> Result<()> {
        let o = RecordingOpener::new();
        let d = deps(t, &o);
        let c = ctx(true);
        let r = run(&c, &d, cmd);
        cleanup(&c);
        r
    }

    /// Field names from `ProviderStatus` in the spec. A renderer reading
    /// invented names still prints a well-formed table of dashes, which no
    /// request-shape test can see.
    #[test]
    fn provider_rows_read_the_field_names_the_api_actually_returns() {
        let body = serde_json::json!({
            "providers": [
                {"provider": "stripe", "label": "Stripe", "connected": true, "mappedPrices": 3}
            ],
            "stripe": {"connected": true, "live": {"connected": true, "accountId": "acct_1"}}
        });
        let rows = provider_rows(&body);
        assert_eq!(rows, vec![vec!["stripe", "Stripe", "true", "3"]]);
        assert!(!rows[0].iter().any(|c| c == "-"));
    }

    /// The overview is an object, never a bare array — reading it as one
    /// yields nothing, which is the bug this replaced.
    #[test]
    fn a_bare_array_is_not_the_overview_shape() {
        let body = serde_json::json!([{"provider": "stripe"}]);
        assert!(
            provider_rows(&body).is_empty(),
            "GET /v1/integrations answers {{providers, stripe}}, not an array"
        );
    }

    /// Stripe has its own path. Routing it through the generic
    /// `/v1/integrations/{provider}/connect` would 404 or misbehave.
    #[test]
    fn connecting_stripe_uses_the_dedicated_oauth_path() {
        let t = transport(vec![resp(200, r#"{"url":"https://connect.stripe"}"#, None)]);
        run_cmd(
            &t,
            IntegrationsCommand::Connect {
                provider: "stripe".into(),
                mode: Some("test".into()),
                environment: None,
                api_key_env: None,
                secret_env: None,
                webhook_secret_env: None,
            },
        )
        .unwrap();

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(
            url.ends_with("/v1/integrations/stripe/connect"),
            "got {url}"
        );
        assert_eq!(body.unwrap(), r#"{"mode":"test"}"#);
    }

    /// Every other provider goes through the generic path with credentials.
    #[test]
    fn connecting_another_provider_uses_the_generic_path() {
        // SAFETY: uniquely named var, removed below.
        unsafe { std::env::set_var("KL_TEST_PADDLE_KEY", "pk_live_x") };
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            IntegrationsCommand::Connect {
                provider: "paddle".into(),
                mode: None,
                environment: Some("sandbox".into()),
                api_key_env: Some("KL_TEST_PADDLE_KEY".into()),
                secret_env: None,
                webhook_secret_env: None,
            },
        )
        .unwrap();
        unsafe { std::env::remove_var("KL_TEST_PADDLE_KEY") };

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(
            url.ends_with("/v1/integrations/paddle/connect"),
            "got {url}"
        );
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["apiKey"], "pk_live_x");
        assert_eq!(sent["environment"], "sandbox");
        assert!(
            !sent.as_object().unwrap().contains_key("secret"),
            "a credential whose env var was not named must be absent"
        );
    }

    /// The provider name is matched case-insensitively — "Stripe" must not
    /// silently take the generic path and skip OAuth.
    #[test]
    fn stripe_routing_is_case_insensitive() {
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            IntegrationsCommand::Connect {
                provider: "Stripe".into(),
                mode: None,
                environment: None,
                api_key_env: None,
                secret_env: None,
                webhook_secret_env: None,
            },
        )
        .unwrap();
        let (_, url, _) = request_at(&t, 0);
        assert!(
            url.ends_with("/v1/integrations/stripe/connect"),
            "got {url}"
        );
    }

    #[test]
    fn disconnect_passes_the_environment_as_a_query_param() {
        let t = transport(vec![resp(200, "{}", None)]);
        run_cmd(
            &t,
            IntegrationsCommand::Disconnect {
                provider: "paddle".into(),
                environment: Some("sandbox".into()),
            },
        )
        .unwrap();

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "DELETE");
        assert!(
            url.contains("/v1/integrations/paddle?environment=sandbox"),
            "got {url}"
        );
    }

    #[test]
    fn rotate_secret_walks_the_confirmation_flow() {
        let pending = r#"{"confirmation":{"id":"c1","status":"pending","approveUrl":"https://app.keylight.dev/c/c1","expiresInSeconds":600}}"#;
        let t = transport(vec![
            resp(202, pending, None),
            resp(200, r#"{"status":"approved"}"#, Some(0)),
            resp(200, r#"{"webhookSecret":"whsec_new"}"#, None),
        ]);
        run_cmd(
            &t,
            IntegrationsCommand::RotateSecret {
                provider: "paddle".into(),
            },
        )
        .unwrap();

        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(
            reqs[2].1.ends_with("/v1/integrations/paddle/rotate-secret"),
            "got {}",
            reqs[2].1
        );
    }
}
