//! `keylight products ...` — apps and their key types.
//!
//! Flag names follow the API's field names rather than the design spec's
//! sketch: creating a product takes `displayName` / `keyPrefix` /
//! `supportEmail`, and there is no client-supplied id at all (the server
//! assigns `productId`). The spec's `--id`/`--name` would have 400'd.

pub mod key_types;

use super::{Context, Deps, json_body, run_gated};
use crate::error::Result;
use crate::output::{self, emit};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ProductsCommand {
    /// List apps.
    List {
        /// Maximum number of apps to return.
        #[arg(long)]
        limit: Option<u32>,
        /// Continue from a previous page's cursor.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one app.
    Get {
        /// The app's id.
        product_id: String,
    },
    /// Create an app.
    Create {
        /// Human-readable app name, e.g. "My App".
        #[arg(long)]
        display_name: String,
        /// Prefix stamped on every key this app issues, e.g. "CLMD".
        #[arg(long)]
        key_prefix: String,
        /// Support address shown to customers.
        #[arg(long)]
        support_email: String,
        /// Where customers go to buy.
        #[arg(long)]
        purchase_url: Option<String>,
        /// Which SDK this app uses.
        #[arg(long)]
        sdk: Option<String>,
    },
    /// Change an app's support email or purchase URL. Those are the only two
    /// fields the API accepts on update.
    Update {
        product_id: String,
        #[arg(long)]
        support_email: Option<String>,
        #[arg(long)]
        purchase_url: Option<String>,
    },
    /// Turn the free tier on or off.
    FreeTier {
        #[command(subcommand)]
        command: FreeTierCommand,
    },
    /// Manage Stripe test mode.
    TestMode {
        #[command(subcommand)]
        command: TestModeCommand,
    },
    /// Configure a payment provider for this app.
    Integrations {
        #[command(subcommand)]
        command: ProductIntegrationsCommand,
    },
    /// Manage this app's license key types.
    KeyTypes {
        #[command(subcommand)]
        command: key_types::KeyTypesCommand,
    },
}

#[derive(Subcommand)]
pub enum FreeTierCommand {
    /// Enable or disable the free tier.
    Set {
        product_id: String,
        /// Turn the free tier on.
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        /// Turn the free tier off.
        #[arg(long)]
        disabled: bool,
    },
}

#[derive(Subcommand)]
pub enum TestModeCommand {
    /// Enable Stripe test mode.
    ///
    /// The two Stripe secrets are read from the environment, never from a
    /// flag: argv is visible in shell history and to other users via `ps`.
    /// This is the same rule that puts `auth set-token` on stdin.
    Enable {
        product_id: String,
        /// Env var holding the Stripe test secret key.
        #[arg(long, default_value = "KEYLIGHT_STRIPE_TEST_SECRET_KEY")]
        secret_key_env: String,
        /// Env var holding the Stripe test webhook secret.
        #[arg(long, default_value = "KEYLIGHT_STRIPE_TEST_WEBHOOK_SECRET")]
        webhook_secret_env: String,
    },
    /// Disable Stripe test mode.
    Disable { product_id: String },
}

#[derive(Subcommand)]
pub enum ProductIntegrationsCommand {
    /// Enable/disable a provider for this app and map its prices.
    Set {
        product_id: String,
        /// Provider name, e.g. `stripe`, `paddle`, `lemonsqueezy`, `polar`.
        provider: String,
        #[arg(long, conflicts_with = "disabled")]
        enabled: bool,
        #[arg(long)]
        disabled: bool,
        /// Price map as JSON, e.g. '{"pro":"price_123"}'.
        #[arg(long)]
        prices: Option<String>,
        /// Per-environment config as JSON.
        #[arg(long)]
        environments: Option<String>,
    },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: ProductsCommand) -> Result<()> {
    match cmd {
        ProductsCommand::List { limit, cursor } => list(ctx, deps, limit, cursor),
        ProductsCommand::Get { product_id } => get(ctx, deps, &product_id),
        ProductsCommand::Create {
            display_name,
            key_prefix,
            support_email,
            purchase_url,
            sdk,
        } => create(
            ctx,
            deps,
            &display_name,
            &key_prefix,
            &support_email,
            purchase_url,
            sdk,
        ),
        ProductsCommand::Update {
            product_id,
            support_email,
            purchase_url,
        } => update(ctx, deps, &product_id, support_email, purchase_url),
        ProductsCommand::FreeTier { command } => match command {
            FreeTierCommand::Set {
                product_id,
                enabled,
                disabled,
            } => free_tier(ctx, deps, &product_id, enabled, disabled),
        },
        ProductsCommand::TestMode { command } => match command {
            TestModeCommand::Enable {
                product_id,
                secret_key_env,
                webhook_secret_env,
            } => test_mode_enable(ctx, deps, &product_id, &secret_key_env, &webhook_secret_env),
            TestModeCommand::Disable { product_id } => test_mode_disable(ctx, deps, &product_id),
        },
        ProductsCommand::Integrations { command } => match command {
            ProductIntegrationsCommand::Set {
                product_id,
                provider,
                enabled,
                disabled,
                prices,
                environments,
            } => integrations_set(
                ctx,
                deps,
                &product_id,
                &provider,
                enabled,
                disabled,
                prices,
                environments,
            ),
        },
        ProductsCommand::KeyTypes { command } => key_types::run(ctx, deps, command),
    }
}

fn list(ctx: &Context, deps: &Deps, limit: Option<u32>, cursor: Option<String>) -> Result<()> {
    let client = ctx.client(deps)?;
    let path = super::query_path(
        "/v1/products",
        &[("limit", limit.map(|l| l.to_string())), ("cursor", cursor)],
    );
    let value = run_gated(ctx, deps, &client, "GET", &path, None)?;
    emit(ctx.json, &value, |v| {
        let items = v.get("items").and_then(|i| i.as_array());
        match items {
            Some(items) if !items.is_empty() => {
                let rows: Vec<Vec<String>> = items
                    .iter()
                    .map(|p| {
                        vec![
                            output::cell(p.get("productId")),
                            output::cell(p.get("displayName")),
                            output::cell(p.get("keyPrefix")),
                            output::cell(p.get("freeTierEnabled")),
                            output::cell(p.get("testModeEnabled")),
                        ]
                    })
                    .collect();
                output::table(&["ID", "NAME", "PREFIX", "FREE TIER", "TEST MODE"], &rows);
                output::next_cursor_hint(v, "keylight products list");
            }
            _ => output::empty(
                "No apps yet. Create one with: keylight products create \
                 --display-name <name> --key-prefix <PREFIX> --support-email <email>",
            ),
        }
    });
    Ok(())
}

fn get(ctx: &Context, deps: &Deps, product_id: &str) -> Result<()> {
    let client = ctx.client(deps)?;
    let value = run_gated(
        ctx,
        deps,
        &client,
        "GET",
        &format!("/v1/products/{product_id}"),
        None,
    )?;
    emit(ctx.json, &value, print_product);
    Ok(())
}

fn create(
    ctx: &Context,
    deps: &Deps,
    display_name: &str,
    key_prefix: &str,
    support_email: &str,
    purchase_url: Option<String>,
    sdk: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let body = json_body(vec![
        ("displayName", Some(display_name.into())),
        ("keyPrefix", Some(key_prefix.into())),
        ("supportEmail", Some(support_email.into())),
        ("purchaseURL", purchase_url.map(Into::into)),
        ("sdk", sdk.map(Into::into)),
    ]);
    let value = run_gated(ctx, deps, &client, "POST", "/v1/products", Some(&body))?;
    emit(ctx.json, &value, print_product);
    Ok(())
}

fn update(
    ctx: &Context,
    deps: &Deps,
    product_id: &str,
    support_email: Option<String>,
    purchase_url: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let body = json_body(vec![
        ("supportEmail", support_email.map(Into::into)),
        ("purchaseURL", purchase_url.map(Into::into)),
    ]);
    let value = run_gated(
        ctx,
        deps,
        &client,
        "PATCH",
        &format!("/v1/products/{product_id}"),
        Some(&body),
    )?;
    emit(ctx.json, &value, print_product);
    Ok(())
}

fn free_tier(
    ctx: &Context,
    deps: &Deps,
    product_id: &str,
    enabled: bool,
    disabled: bool,
) -> Result<()> {
    let client = ctx.client(deps)?;
    // `enabled` is required by the API, so the command must say which way it
    // is going rather than defaulting — silently disabling a live free tier
    // because a flag was forgotten is not an acceptable default.
    let on = super::require_toggle(enabled, disabled, "--enabled", "--disabled")?;
    let body = serde_json::json!({ "enabled": on });
    let value = run_gated(
        ctx,
        deps,
        &client,
        "POST",
        &format!("/v1/products/{product_id}/free-tier"),
        Some(&body),
    )?;
    // The route answers `{product}`, so show the resulting state rather than
    // a bare success line — the human sees what changed and an agent doesn't
    // need a second call to find out.
    emit(ctx.json, &value, |v| {
        println!(
            "Free tier {} for {product_id}.",
            if on { "enabled" } else { "disabled" }
        );
        println!();
        print_product(v);
    });
    Ok(())
}

fn test_mode_enable(
    ctx: &Context,
    deps: &Deps,
    product_id: &str,
    secret_key_env: &str,
    webhook_secret_env: &str,
) -> Result<()> {
    let client = ctx.client(deps)?;
    let secret = super::secret_from_env(secret_key_env)?;
    let webhook_secret = super::secret_from_env(webhook_secret_env)?;
    let body = serde_json::json!({
        "stripeTestSecretKey": secret,
        "stripeTestWebhookSecret": webhook_secret,
    });
    let value = run_gated(
        ctx,
        deps,
        &client,
        "POST",
        &format!("/v1/products/{product_id}/test-mode"),
        Some(&body),
    )?;
    emit(ctx.json, &value, |v| {
        println!("Test mode enabled for {product_id}.");
        println!();
        print_product(v);
    });
    Ok(())
}

fn test_mode_disable(ctx: &Context, deps: &Deps, product_id: &str) -> Result<()> {
    let client = ctx.client(deps)?;
    let value = run_gated(
        ctx,
        deps,
        &client,
        "DELETE",
        &format!("/v1/products/{product_id}/test-mode"),
        None,
    )?;
    emit(ctx.json, &value, |v| {
        println!("Test mode disabled for {product_id}.");
        println!();
        print_product(v);
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn integrations_set(
    ctx: &Context,
    deps: &Deps,
    product_id: &str,
    provider: &str,
    enabled: bool,
    disabled: bool,
    prices: Option<String>,
    environments: Option<String>,
) -> Result<()> {
    let client = ctx.client(deps)?;
    // Unlike free-tier, `enabled` is optional here — omitting both flags
    // means "leave the enabled state alone and just update the prices".
    let on = match (enabled, disabled) {
        (true, false) => Some(serde_json::Value::Bool(true)),
        (false, true) => Some(serde_json::Value::Bool(false)),
        _ => None,
    };
    let body = json_body(vec![
        ("enabled", on),
        ("prices", super::parse_json_flag(prices, "--prices")?),
        (
            "environments",
            super::parse_json_flag(environments, "--environments")?,
        ),
    ]);
    let value = run_gated(
        ctx,
        deps,
        &client,
        "POST",
        &format!("/v1/products/{product_id}/integrations/{provider}"),
        Some(&body),
    )?;
    // Answers {productId, provider, enabled, mappedPrices} — the mapped-price
    // count is the whole reason to run this, so print it.
    emit(ctx.json, &value, |v| {
        output::kv(&[
            ("App", output::cell(v.get("productId"))),
            ("Provider", output::cell(v.get("provider"))),
            ("Enabled", output::cell(v.get("enabled"))),
            ("Mapped prices", output::cell(v.get("mappedPrices"))),
        ]);
    });
    Ok(())
}

fn print_product(v: &serde_json::Value) {
    // Some routes answer `{product: {...}}` and some answer the object
    // directly; unwrap one level so the renderer works for both.
    let p = v.get("product").unwrap_or(v);
    output::kv(&[
        ("ID", output::cell(p.get("productId"))),
        ("Name", output::cell(p.get("displayName"))),
        ("Key prefix", output::cell(p.get("keyPrefix"))),
        ("Support email", output::cell(p.get("supportEmail"))),
        ("Purchase URL", output::cell(p.get("purchaseURL"))),
        ("Free tier", output::cell(p.get("freeTierEnabled"))),
        ("Test mode", output::cell(p.get("testModeEnabled"))),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, request_at, transport};
    use crate::test_support::resp;

    const PRODUCT: &str = r#"{"productId":"my-app","displayName":"My App","keyPrefix":"MYAP","supportEmail":"help@x.com","freeTierEnabled":false,"testModeEnabled":false}"#;

    /// The design spec advertised `--id`/`--name`. The endpoint requires
    /// displayName + keyPrefix + supportEmail and assigns the id itself, so
    /// the spec's flags would have produced a 400 on every create.
    #[test]
    fn create_sends_the_fields_the_endpoint_actually_requires() {
        let t = transport(vec![resp(201, PRODUCT, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        create(
            &c,
            &d,
            "My App",
            "MYAP",
            "help@x.com",
            Some("https://buy.example".into()),
            None,
        )
        .unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/products"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["displayName"], "My App");
        assert_eq!(sent["keyPrefix"], "MYAP");
        assert_eq!(sent["supportEmail"], "help@x.com");
        assert_eq!(sent["purchaseURL"], "https://buy.example");
        assert!(
            !sent.as_object().unwrap().contains_key("sdk"),
            "an unsupplied optional must be absent, not null"
        );
        assert!(
            !sent.as_object().unwrap().contains_key("id"),
            "there is no client-supplied id field on this endpoint"
        );
    }

    #[test]
    fn list_passes_limit_and_cursor_through_as_query_params() {
        let t = transport(vec![resp(200, r#"{"items":[],"nextCursor":null}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        list(&c, &d, Some(25), Some("cur+sor".into())).unwrap();
        cleanup(&c);

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "GET");
        assert!(url.contains("limit=25"), "got {url}");
        assert!(
            url.contains("cursor=cur%2Bsor"),
            "the cursor must be percent-encoded: {url}"
        );
    }

    /// Update accepts only supportEmail and purchaseURL. An omitted flag must
    /// not reach the wire — PATCH treats a present null as "clear this".
    #[test]
    fn update_omits_the_field_that_was_not_passed() {
        let t = transport(vec![resp(200, PRODUCT, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        update(&c, &d, "my-app", Some("new@x.com".into()), None).unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "PATCH");
        assert!(url.ends_with("/v1/products/my-app"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent, serde_json::json!({"supportEmail": "new@x.com"}));
    }

    #[test]
    fn free_tier_sends_the_required_enabled_boolean() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        free_tier(&c, &d, "my-app", true, false).unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/products/my-app/free-tier"), "got {url}");
        assert_eq!(body.unwrap(), r#"{"enabled":true}"#);
    }

    /// Neither flag must not silently disable a live free tier.
    #[test]
    fn free_tier_with_neither_flag_makes_no_request_at_all() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let err = free_tier(&c, &d, "my-app", false, false).unwrap_err();
        cleanup(&c);
        assert!(err.to_string().contains("--enabled"), "got: {err}");
        assert!(
            t.requests.lock().unwrap().is_empty(),
            "must not call the API when the intent is ambiguous"
        );
    }

    /// Stripe secrets are read from the environment, never from argv — argv
    /// is visible in shell history and to other users via `ps`.
    #[test]
    fn test_mode_enable_reads_both_secrets_from_the_environment() {
        // SAFETY: single-threaded within this test; the vars are uniquely
        // named so no other test observes them.
        unsafe {
            std::env::set_var("KL_TEST_SECRET_A", "sk_test_123");
            std::env::set_var("KL_TEST_SECRET_B", "whsec_456");
        }
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        test_mode_enable(&c, &d, "my-app", "KL_TEST_SECRET_A", "KL_TEST_SECRET_B").unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/products/my-app/test-mode"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert_eq!(sent["stripeTestSecretKey"], "sk_test_123");
        assert_eq!(sent["stripeTestWebhookSecret"], "whsec_456");
        unsafe {
            std::env::remove_var("KL_TEST_SECRET_A");
            std::env::remove_var("KL_TEST_SECRET_B");
        }
    }

    /// A missing secret must fail before any network call, with a message
    /// naming the variable to set.
    #[test]
    fn test_mode_enable_without_the_env_var_fails_before_calling_the_api() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let err = test_mode_enable(&c, &d, "my-app", "KL_DEFINITELY_UNSET_VAR", "ALSO_UNSET")
            .unwrap_err();
        cleanup(&c);
        assert!(
            err.to_string().contains("KL_DEFINITELY_UNSET_VAR"),
            "the message must name the variable: {err}"
        );
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn test_mode_disable_is_a_delete() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        test_mode_disable(&c, &d, "my-app").unwrap();
        cleanup(&c);

        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "DELETE");
        assert!(url.ends_with("/v1/products/my-app/test-mode"), "got {url}");
    }

    /// Omitting both toggles means "update prices, leave enabled alone" —
    /// unlike free-tier, `enabled` is optional on this endpoint.
    #[test]
    fn product_integrations_omits_enabled_when_neither_flag_is_given() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        integrations_set(
            &c,
            &d,
            "my-app",
            "stripe",
            false,
            false,
            Some(r#"{"pro":"price_123"}"#.into()),
            None,
        )
        .unwrap();
        cleanup(&c);

        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "POST");
        assert!(
            url.ends_with("/v1/products/my-app/integrations/stripe"),
            "got {url}"
        );
        let sent: serde_json::Value = serde_json::from_str(&body.unwrap()).unwrap();
        assert!(!sent.as_object().unwrap().contains_key("enabled"));
        // The flag's JSON is forwarded as JSON, not as a quoted string.
        assert_eq!(sent["prices"]["pro"], "price_123");
    }
}
