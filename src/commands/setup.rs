//! `keylight setup …` — checks a human runs once, at setup time.

use super::{Context, Deps, run_gated, secret_from_env};
use crate::api::{ApiClient, ApiOutcome};
use crate::error::{CliError, Result};
use clap::Subcommand;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SETUP_CHECK: &str = "Setup check";
/// The device_id the API connection test activates under. A fixed, obviously
/// synthetic value so the row is recognizable and a re-run reuses the same seat.
const SETUP_DEVICE_ID: &str = "keylight-setup-check";
/// The env var holding the app's `licenses:runtime` token for the API test.
/// Deliberately distinct from `KEYLIGHT_API_TOKEN` (the CLI's own management
/// auth): the two are different tokens with different scopes, and reading the
/// runtime token from the management var would either shadow the login or fail
/// minting with a scope error. `keylight init` writes both and names this one.
const RUNTIME_TOKEN_ENV: &str = "KEYLIGHT_RUNTIME_TOKEN";

#[derive(Subcommand)]
pub enum SetupCommand {
    /// Issue a test license, activate it, and verify the lease. This proves
    /// the tenant, app, SDK key and signing key work end to end.
    VerifyLicense {
        /// App to verify. Optional when the tenant has exactly one.
        #[arg(long)]
        product: Option<String>,
        /// Delete the test license and device without asking.
        #[arg(long, conflicts_with = "keep")]
        clean: bool,
        /// Keep the test license and device without asking.
        #[arg(long)]
        keep: bool,
    },
    /// Prove an API-mode app works end to end over plain HTTPS — no SDK. Mints a
    /// test license with your CLI login, then activates and validates it through
    /// the `/v1/licenses` runtime endpoints using your server-side
    /// `licenses:runtime` token, prints the JSON, and cleans up.
    VerifyApi {
        /// App to verify. Optional when the tenant has exactly one.
        #[arg(long)]
        product: Option<String>,
        /// Env var holding the app's `licenses:runtime` bearer token. Read from
        /// the environment, never a flag, so it stays out of shell history.
        #[arg(long, default_value = RUNTIME_TOKEN_ENV)]
        token_env: String,
        /// device_id to activate under. Defaults to a fixed setup-check id.
        #[arg(long)]
        device_id: Option<String>,
        /// Keep the test license and device instead of cleaning them up.
        #[arg(long)]
        keep: bool,
    },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: SetupCommand) -> Result<()> {
    match cmd {
        SetupCommand::VerifyLicense {
            product,
            clean,
            keep,
        } => verify_license(ctx, deps, product.as_deref(), clean, keep),
        SetupCommand::VerifyApi {
            product,
            token_env,
            device_id,
            keep,
        } => verify_api(ctx, deps, product.as_deref(), &token_env, device_id, keep),
    }
}

fn json_ok(outcome: ApiOutcome) -> Result<serde_json::Value> {
    match outcome {
        ApiOutcome::Ok { value, .. } => Ok(value),
        // These reads and the mint route are not confirmation-gated. A 202
        // means the server is not the API this CLI was built for.
        ApiOutcome::Pending(_) => Err(CliError::InvalidResponse),
    }
}

/// Which app to verify. An explicit `--product` wins. Otherwise the tenant's
/// single app is used; picking between several would consume a seat on an app
/// the operator did not choose.
fn resolve_product(ctx: &Context, deps: &Deps, explicit: Option<&str>) -> Result<String> {
    if let Some(product) = explicit {
        return Ok(product.to_string());
    }
    let client = ctx.client(deps)?;
    let body = json_ok(client.get("/v1/products")?)?;
    let items = body["items"].as_array().cloned().unwrap_or_default();
    match items.len() {
        0 => Err(CliError::UsageError(
            "This tenant has no apps. Create one with: keylight products create".into(),
        )),
        1 => items[0]["productId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .ok_or(CliError::InvalidResponse),
        _ => {
            let names: Vec<String> = items
                .iter()
                .map(|p| p["productId"].as_str().unwrap_or("?").to_string())
                .collect();
            Err(CliError::UsageError(format!(
                "This tenant has several apps ({}). Pass --product <id>.",
                names.join(", ")
            )))
        }
    }
}

/// The SDK key the activation authenticates with.
///
/// The environment is the normal path: fetching over the API is
/// confirmation-gated, and setup already asked a human to approve it. A
/// missing key in a script must fail with a useful next action instead of
/// waiting forever for an approval nobody can click.
fn resolve_sdk_key(ctx: &Context, deps: &Deps) -> Result<String> {
    if let Ok(key) = std::env::var("KEYLIGHT_SDK_KEY") {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    if !ctx.is_tty || ctx.json {
        return Err(CliError::UsageError(
            "No SDK key. Run `keylight sdk-key get` (a human approves it in the \
             browser), then set KEYLIGHT_SDK_KEY and re-run this command."
                .into(),
        ));
    }

    let client = ctx.client(deps)?;
    let value = run_gated(ctx, deps, &client, "GET", "/v1/sdk-key", None)?;
    value["sdkKey"]
        .as_str()
        .filter(|key| !key.trim().is_empty())
        .map(str::to_string)
        .ok_or(CliError::InvalidResponse)
}

fn resolve_tenant_id(ctx: &Context, deps: &Deps) -> Result<String> {
    let client = ctx.client(deps)?;
    let value = json_ok(client.get("/v1/tokens/verify")?)?;
    value["tenantId"]
        .as_str()
        .filter(|tenant| !tenant.is_empty())
        .map(str::to_string)
        .ok_or(CliError::InvalidResponse)
}

struct VerifyOutcome {
    license_id: String,
    display_key: String,
    instance_id: Option<String>,
    product_id: String,
}

/// The one plaintext key that the API deliberately returns only at mint time.
/// It lives in a second owner-only config file only until `--clean` succeeds,
/// which lets a retry reuse one license instead of quietly creating a trail.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SetupCheck {
    tenant_id: String,
    product_id: String,
    license_id: String,
    license_key: String,
    display_key: String,
    instance_id: Option<String>,
}

fn state_path(ctx: &Context) -> PathBuf {
    ctx.config_path.with_extension("setup-check.json")
}

fn load_setup_check(
    ctx: &Context,
    tenant_id: &str,
    product_id: &str,
) -> Result<Option<SetupCheck>> {
    let path = state_path(ctx);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let check: SetupCheck = serde_json::from_str(&raw).map_err(|error| {
                CliError::Config(format!(
                    "invalid setup-check state at {}: {error}",
                    path.display()
                ))
            })?;
            Ok((check.tenant_id == tenant_id && check.product_id == product_id).then_some(check))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CliError::Io(error)),
    }
}

fn save_setup_check(ctx: &Context, check: &SetupCheck) -> Result<()> {
    let path = state_path(ctx);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec(check)
        .map_err(|error| CliError::Config(format!("could not store setup-check state: {error}")))?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&body)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, body)?;
    Ok(())
}

fn clear_setup_check(ctx: &Context) -> Result<()> {
    match std::fs::remove_file(state_path(ctx)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CliError::Io(error)),
    }
}

/// A short-lived in-process store. The check has no user-facing state to
/// preserve, so it must not leave a cached real license in the CLI user's SDK
/// directory merely by proving an activation works.
#[derive(Default)]
struct EphemeralStore(Mutex<HashMap<String, Vec<u8>>>);

impl keylight::LicenseStore for EphemeralStore {
    fn get(&self, account: &str) -> Option<Vec<u8>> {
        self.0.lock().ok()?.get(account).cloned()
    }

    fn set(&self, account: &str, value: &[u8]) -> keylight::Result<()> {
        self.0
            .lock()
            .map_err(|_| {
                keylight::KeylightError::Storage("setup check storage lock poisoned".into())
            })?
            .insert(account.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, account: &str) -> keylight::Result<()> {
        self.0
            .lock()
            .map_err(|_| {
                keylight::KeylightError::Storage("setup check storage lock poisoned".into())
            })?
            .remove(account);
        Ok(())
    }
}

/// Issue the throwaway license. Its customer name makes the row recognizable
/// in the dashboard, and email remains opt-in (therefore absent here).
fn mint_setup_license(
    ctx: &Context,
    deps: &Deps,
    tenant_id: &str,
    product: &str,
) -> Result<SetupCheck> {
    let client = ctx.client(deps)?;
    let body = serde_json::json!({
        "productId": product,
        "customerName": SETUP_CHECK,
    });
    let value = json_ok(client.call("POST", "/v1/licenses", Some(&body), None)?)?;
    let license_id = value["license"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(CliError::InvalidResponse)?
        .to_string();
    let key = value["licenseKey"]
        .as_str()
        .filter(|key| !key.is_empty())
        .ok_or(CliError::InvalidResponse)?
        .to_string();
    let display = value["license"]["displayKey"]
        .as_str()
        .filter(|key| !key.is_empty())
        .ok_or(CliError::InvalidResponse)?
        .to_string();
    Ok(SetupCheck {
        tenant_id: tenant_id.to_string(),
        product_id: product.to_string(),
        license_id,
        license_key: key,
        display_key: display,
        instance_id: None,
    })
}

fn reuse_or_mint(
    ctx: &Context,
    deps: &Deps,
    tenant_id: &str,
    product_id: &str,
) -> Result<SetupCheck> {
    if let Some(check) = load_setup_check(ctx, tenant_id, product_id)? {
        return Ok(check);
    }
    let check = mint_setup_license(ctx, deps, tenant_id, product_id)?;
    save_setup_check(ctx, &check)?;
    Ok(check)
}

/// Activate through the real SDK rather than a hand-rolled HTTP call. That
/// forces the returned lease through its normal keyset and Ed25519 validation.
fn verify_license(
    ctx: &Context,
    deps: &Deps,
    product: Option<&str>,
    clean: bool,
    keep: bool,
) -> Result<()> {
    let product_id = resolve_product(ctx, deps, product)?;
    let tenant_id = resolve_tenant_id(ctx, deps)?;
    // A later `--clean` must work from the management token alone. Requiring
    // an SDK key just to revoke an already-recorded setup license would make
    // the safe reset path depend on an unrelated secret still being present.
    if clean {
        if let Some(check) = load_setup_check(ctx, &tenant_id, &product_id)? {
            return offer_cleanup(ctx, deps, &outcome_from_check(&check), true, false);
        }
    }
    let mut check = reuse_or_mint(ctx, deps, &tenant_id, &product_id)?;
    if check.instance_id.is_some() {
        let outcome = outcome_from_check(&check);
        report(ctx, &outcome);
        return offer_cleanup(ctx, deps, &outcome, false, keep);
    }
    let sdk_key = resolve_sdk_key(ctx, deps)?;

    let mut config = keylight::KeylightConfig::builder(&tenant_id, &product_id, sdk_key)
        .app_version("setup-check")
        .base_url(&ctx.api_url)
        .build();
    let transport = keylight::http::ureq_transport::UreqTransport::default();
    match keylight::keyset::fetch_keyset(&transport, &config.base_url, &config.tenant_id) {
        Some((_, keys)) => config.trusted_keys.extend(keys),
        None => {
            return Err(CliError::VerificationFailed(
                "could not fetch the tenant keyset".into(),
            ));
        }
    }

    let sdk = keylight::Keylight::with_parts(
        config,
        Arc::new(EphemeralStore::default()),
        Arc::new(transport),
    );
    let activation = sdk
        .activate(&check.license_key)
        .map_err(|error| CliError::VerificationFailed(error.to_string()))?;
    if !activation.activated {
        return Err(CliError::VerificationFailed(
            activation
                .error
                .unwrap_or_else(|| "activation was refused".into()),
        ));
    }
    let instance_id = activation.instance_id.ok_or_else(|| {
        CliError::VerificationFailed("activation succeeded without an instance id".into())
    })?;
    check.instance_id = Some(instance_id);
    save_setup_check(ctx, &check)?;

    let outcome = outcome_from_check(&check);
    report(ctx, &outcome);
    offer_cleanup(ctx, deps, &outcome, clean, keep)
}

// --------------------------------------------------------------------------
// API-mode connection test (`setup verify-api`)
//
// The HTTPS-only analog of verify-license: no Keylight SDK is imported.
// Minting and cleanup use the CLI's management login; the activate/validate
// pair uses the app's own `licenses:runtime` token, exercising the exact path
// a server-side integration takes.
// --------------------------------------------------------------------------

fn verify_api(
    ctx: &Context,
    deps: &Deps,
    product: Option<&str>,
    token_env: &str,
    device_id: Option<String>,
    keep: bool,
) -> Result<()> {
    let runtime_token = secret_from_env(token_env)?;
    let management = ctx.client(deps)?;
    let product_id = resolve_product(ctx, deps, product)?;
    let tenant_id = resolve_tenant_id(ctx, deps)?;
    let device_id = device_id.unwrap_or_else(|| SETUP_DEVICE_ID.to_string());

    // A throwaway license, minted with the management login.
    let check = mint_setup_license(ctx, deps, &tenant_id, &product_id)?;

    // The runtime client speaks the clean-JSON /v1/licenses surface with the
    // app's own token — a different credential and scope from the management
    // login above, which is exactly what this command exists to prove.
    let runtime = ApiClient::new(
        deps.transport.clone(),
        deps.sleeper.clone(),
        ctx.api_url.clone(),
        runtime_token,
    );

    let proof = run_api_proof(&runtime, &product_id, &check.license_key, &device_id);

    // Always tidy up unless asked to keep — even when the proof failed, so a
    // refused activation never leaves an orphaned license behind. Best-effort:
    // a cleanup failure is a warning, since the proof's own result is what
    // this command reports.
    if !keep {
        let body = runtime_body(&product_id, &check.license_key, &device_id);
        if let Err(error) = runtime.call("POST", "/v1/licenses/deactivate", Some(&body), None) {
            eprintln!("Could not release the setup-check device: {error}");
        }
        if let Err(error) = run_gated(
            ctx,
            deps,
            &management,
            "POST",
            &format!("/v1/licenses/{}/revoke", check.license_id),
            None,
        ) {
            eprintln!("Could not revoke the setup-check license: {error}");
        }
    }

    let (activate, validate) = proof?;
    report_api(
        ctx,
        &product_id,
        &check.display_key,
        &activate,
        &validate,
        keep,
    );
    Ok(())
}

/// The shared request body for the three runtime endpoints — the clean-JSON
/// contract is snake_case `product_id` / `license_key` / `device_id`.
fn runtime_body(product_id: &str, license_key: &str, device_id: &str) -> serde_json::Value {
    serde_json::json!({
        "product_id": product_id,
        "license_key": license_key,
        "device_id": device_id,
    })
}

/// Activate, then validate, over HTTPS. Returns both responses on success. A
/// clean `valid:false` is still a failed proof — the point is a usable license,
/// not merely a well-formed answer.
fn run_api_proof(
    runtime: &ApiClient,
    product_id: &str,
    license_key: &str,
    device_id: &str,
) -> Result<(serde_json::Value, serde_json::Value)> {
    let body = runtime_body(product_id, license_key, device_id);
    let activate = json_ok(runtime.call("POST", "/v1/licenses/activate", Some(&body), None)?)?;
    let validate = json_ok(runtime.call("POST", "/v1/licenses/validate", Some(&body), None)?)?;
    if validate.get("valid").and_then(|v| v.as_bool()) != Some(true) {
        return Err(CliError::VerificationFailed(format!(
            "the runtime API returned valid=false: {validate}"
        )));
    }
    Ok((activate, validate))
}

fn report_api(
    ctx: &Context,
    product_id: &str,
    display_key: &str,
    activate: &serde_json::Value,
    validate: &serde_json::Value,
    kept: bool,
) {
    crate::output::emit(
        ctx.json,
        &serde_json::json!({
            "verified": true,
            "productId": product_id,
            "activate": activate,
            "validate": validate,
        }),
        |_| {
            println!("Verified over HTTPS. Your API-mode app can license without an SDK.");
            println!();
            println!("  product   {product_id}");
            println!("  activate  {}", compact(activate));
            println!("  validate  {}", compact(validate));
            if kept {
                println!();
                println!(
                    "  test license {display_key} kept (customer: {SETUP_CHECK}). \
                     Delete it from the dashboard when you're done."
                );
            }
        },
    );
}

/// One-line JSON for the human view; the full objects still go out under --json.
fn compact(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn outcome_from_check(check: &SetupCheck) -> VerifyOutcome {
    VerifyOutcome {
        license_id: check.license_id.clone(),
        display_key: check.display_key.clone(),
        instance_id: check.instance_id.clone(),
        product_id: check.product_id.clone(),
    }
}

/// What the run created, so a human can decide whether to keep it.
fn report(ctx: &Context, outcome: &VerifyOutcome) {
    crate::output::emit(
        ctx.json,
        &serde_json::json!({
            "verified": true,
            "productId": outcome.product_id,
            "licenseKey": outcome.display_key,
            "device": SETUP_CHECK,
        }),
        |_| {
            println!("Verified. Keylight is working and your SDK key is ready.");
            println!();
            println!(
                "  license  {}  (customer: {SETUP_CHECK})",
                outcome.display_key
            );
            println!("  device   {SETUP_CHECK}");
            println!();
            println!("These are real. Delete them with: keylight setup verify-license --clean");
        },
    );
}

/// Cleanup is opt-in. Non-interactive runs never infer consent from silence.
fn offer_cleanup(
    ctx: &Context,
    deps: &Deps,
    outcome: &VerifyOutcome,
    clean: bool,
    keep: bool,
) -> Result<()> {
    if keep {
        return Ok(());
    }
    let should_clean =
        clean || (ctx.is_tty && !ctx.json && prompt_yes_no("Delete the test license and device?"));
    if !should_clean {
        return Ok(());
    }

    let client = ctx.client(deps)?;
    // Release the seat before revoking. Each route can be confirm-gated; a
    // cleanup failure remains a warning because verification already passed.
    if let Some(instance_id) = &outcome.instance_id {
        if let Err(error) = run_gated(
            ctx,
            deps,
            &client,
            "POST",
            &format!("/v1/licenses/{}/deactivate-device", outcome.license_id),
            Some(&serde_json::json!({ "instanceId": instance_id })),
        ) {
            eprintln!("Could not release the setup-check device: {error}");
        }
    }
    match run_gated(
        ctx,
        deps,
        &client,
        "POST",
        &format!("/v1/licenses/{}/revoke", outcome.license_id),
        None,
    ) {
        Ok(_) => clear_setup_check(ctx)?,
        Err(error) => eprintln!("Could not revoke the setup-check license: {error}"),
    }
    Ok(())
}

/// Blocking y/N prompt. It is called only when stdout is a terminal.
fn prompt_yes_no(question: &str) -> bool {
    use std::io::{Write, stdin, stdout};

    print!("{question} [y/N] ");
    let _ = stdout().flush();
    let mut line = String::new();
    if stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, cleanup, ctx, deps, transport};
    use crate::test_support::resp;
    use std::sync::Mutex;

    static SDK_KEY_ENV: Mutex<()> = Mutex::new(());

    fn outcome() -> VerifyOutcome {
        VerifyOutcome {
            license_id: "license-id".into(),
            display_key: "FNGR-****-DDDD".into(),
            instance_id: Some("instance-id".into()),
            product_id: "fingerhat".into(),
        }
    }

    #[test]
    fn resolves_the_only_product() {
        let t = transport(vec![resp(
            200,
            r#"{"items":[{"productId":"fingerhat","keyPrefix":"FNGR"}],"nextCursor":null}"#,
            None,
        )]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        assert_eq!(resolve_product(&c, &d, None).unwrap(), "fingerhat");
        cleanup(&c);
    }

    #[test]
    fn refuses_to_guess_between_products() {
        let t = transport(vec![resp(
            200,
            r#"{"items":[{"productId":"a"},{"productId":"b"}],"nextCursor":null}"#,
            None,
        )]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let error = resolve_product(&c, &d, None).unwrap_err();
        cleanup(&c);
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn explicit_product_makes_no_request() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        assert_eq!(resolve_product(&c, &d, Some("chosen")).unwrap(), "chosen");
        cleanup(&c);
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn reads_the_sdk_key_from_the_environment() {
        let _guard = SDK_KEY_ENV.lock().unwrap();
        unsafe { std::env::set_var("KEYLIGHT_SDK_KEY", "sdk_live_abc") };
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        assert_eq!(resolve_sdk_key(&c, &d).unwrap(), "sdk_live_abc");
        cleanup(&c);
        unsafe { std::env::remove_var("KEYLIGHT_SDK_KEY") };
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn errors_when_non_interactive_and_no_sdk_key() {
        let _guard = SDK_KEY_ENV.lock().unwrap();
        unsafe { std::env::remove_var("KEYLIGHT_SDK_KEY") };
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let error = resolve_sdk_key(&c, &d).unwrap_err();
        cleanup(&c);
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("keylight sdk-key get"));
    }

    #[test]
    fn mints_a_license_named_setup_check() {
        let t = transport(vec![resp(
            201,
            r#"{"licenseKey":"FNGR-AAAA-BBBB-CCCC-DDDD","license":{"id":"license-id","displayKey":"FNGR-****-DDDD"}}"#,
            None,
        )]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let check = mint_setup_license(&c, &d, "tenant-1", "fingerhat").unwrap();
        cleanup(&c);
        assert_eq!(check.license_key, "FNGR-AAAA-BBBB-CCCC-DDDD");
        assert_eq!(check.display_key, "FNGR-****-DDDD");
        let body = t.requests.lock().unwrap()[0].3.clone().unwrap();
        assert!(body.contains(SETUP_CHECK));
        assert!(body.contains("fingerhat"));
        assert!(!body.contains("sendEmail"));
    }

    #[test]
    fn non_interactive_never_deletes() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        offer_cleanup(&c, &d, &outcome(), false, false).unwrap();
        cleanup(&c);
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn clean_deactivates_then_revokes() {
        let t = transport(vec![resp(200, r#"{}"#, None), resp(200, r#"{}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        offer_cleanup(&c, &d, &outcome(), true, false).unwrap();
        cleanup(&c);
        let requests = t.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].1.ends_with("/deactivate-device"));
        assert!(requests[1].1.ends_with("/revoke"));
    }

    #[test]
    fn clean_removes_a_saved_check_without_an_sdk_key() {
        let _guard = SDK_KEY_ENV.lock().unwrap();
        unsafe { std::env::remove_var("KEYLIGHT_SDK_KEY") };
        let t = transport(vec![
            resp(200, r#"{"items":[{"productId":"fingerhat"}]}"#, None),
            resp(200, r#"{"tenantId":"tenant-1"}"#, None),
            resp(200, r#"{}"#, None),
            resp(200, r#"{}"#, None),
        ]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        save_setup_check(
            &c,
            &SetupCheck {
                tenant_id: "tenant-1".into(),
                product_id: "fingerhat".into(),
                license_id: "license-id".into(),
                license_key: "FNGR-AAAA".into(),
                display_key: "FNGR-****".into(),
                instance_id: Some("instance-id".into()),
            },
        )
        .unwrap();
        verify_license(&c, &d, None, true, false).unwrap();
        cleanup(&c);
        assert_eq!(t.requests.lock().unwrap().len(), 4);
        assert!(!state_path(&c).exists());
    }

    #[test]
    fn reuses_the_locally_stored_setup_check() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let check = SetupCheck {
            tenant_id: "tenant-1".into(),
            product_id: "fingerhat".into(),
            license_id: "license-id".into(),
            license_key: "FNGR-AAAA".into(),
            display_key: "FNGR-****".into(),
            instance_id: None,
        };
        save_setup_check(&c, &check).unwrap();
        let reused = reuse_or_mint(&c, &d, "tenant-1", "fingerhat").unwrap();
        clear_setup_check(&c).unwrap();
        cleanup(&c);
        assert_eq!(reused.license_key, "FNGR-AAAA");
        assert!(t.requests.lock().unwrap().is_empty());
    }

    // ---- setup verify-api (the HTTPS-only connection test) ----------------

    static RUNTIME_ENV: Mutex<()> = Mutex::new(());

    fn set_runtime_token(v: &str) {
        unsafe { std::env::set_var(RUNTIME_TOKEN_ENV, v) };
    }
    fn clear_runtime_token() {
        unsafe { std::env::remove_var(RUNTIME_TOKEN_ENV) };
    }

    /// The headline path: mint with the login, then activate + validate over
    /// the runtime endpoints with the app's own token. The activate call must
    /// carry the snake_case runtime body and the runtime token — proving the
    /// two credentials are genuinely separate.
    #[test]
    fn verify_api_activates_and_validates_over_https() {
        let _g = RUNTIME_ENV.lock().unwrap();
        set_runtime_token("klm_runtime");
        let t = transport(vec![
            resp(
                200,
                r#"{"items":[{"productId":"fingerhat"}],"nextCursor":null}"#,
                None,
            ),
            resp(200, r#"{"tenantId":"tenant-1"}"#, None),
            resp(
                201,
                r#"{"licenseKey":"FNGR-AAAA-BBBB-CCCC-DDDD","license":{"id":"license-id","displayKey":"FNGR-****-DDDD"}}"#,
                None,
            ),
            resp(200, r#"{"activated":true,"status":"active"}"#, None),
            resp(
                200,
                r#"{"valid":true,"status":"active","entitlements":{}}"#,
                None,
            ),
        ]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        verify_api(&c, &d, None, RUNTIME_TOKEN_ENV, None, true).unwrap();
        cleanup(&c);
        clear_runtime_token();

        let reqs = t.requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            5,
            "products, verify, mint, activate, validate — no cleanup under --keep"
        );
        // The mint uses the management login...
        assert!(reqs[2].1.ends_with("/v1/licenses"));
        assert!(
            reqs[2]
                .2
                .contains(&("Authorization".to_string(), "Bearer klm_test".to_string()))
        );
        // ...and the activate uses the app's runtime token + snake_case body.
        let (method, url, headers, body) = &reqs[3];
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/licenses/activate"), "got {url}");
        let sent: serde_json::Value = serde_json::from_str(body.as_deref().unwrap()).unwrap();
        assert_eq!(sent["product_id"], "fingerhat");
        assert_eq!(sent["license_key"], "FNGR-AAAA-BBBB-CCCC-DDDD");
        assert_eq!(sent["device_id"], SETUP_DEVICE_ID);
        assert!(
            headers.contains(&(
                "Authorization".to_string(),
                "Bearer klm_runtime".to_string()
            )),
            "the runtime call must use the app's token, not the management login"
        );
        assert!(reqs[4].1.ends_with("/v1/licenses/validate"));
    }

    /// A well-formed `valid:false` is still a failed proof — and a failed proof
    /// must not leave the throwaway license behind.
    #[test]
    fn verify_api_treats_valid_false_as_a_failure_and_still_cleans_up() {
        let _g = RUNTIME_ENV.lock().unwrap();
        set_runtime_token("klm_runtime");
        let t = transport(vec![
            resp(200, r#"{"items":[{"productId":"fingerhat"}]}"#, None),
            resp(200, r#"{"tenantId":"tenant-1"}"#, None),
            resp(
                201,
                r#"{"licenseKey":"FNGR-AAAA","license":{"id":"license-id","displayKey":"FNGR-****"}}"#,
                None,
            ),
            resp(200, r#"{"activated":true,"status":"active"}"#, None),
            resp(
                200,
                r#"{"valid":false,"status":"revoked","reason":"revoked"}"#,
                None,
            ),
            resp(200, r#"{"deactivated":true}"#, None),
            resp(200, r#"{}"#, None),
        ]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let err = verify_api(&c, &d, None, RUNTIME_TOKEN_ENV, None, false).unwrap_err();
        cleanup(&c);
        clear_runtime_token();

        assert!(matches!(err, CliError::VerificationFailed(_)));
        let reqs = t.requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            7,
            "must deactivate + revoke even after a failed proof"
        );
        assert!(reqs[5].1.ends_with("/v1/licenses/deactivate"));
        assert!(reqs[6].1.ends_with("/revoke"));
    }

    /// The whole point is HTTPS-with-a-token; without one there's nothing to
    /// test, and it must fail before touching the network.
    #[test]
    fn verify_api_requires_the_runtime_token() {
        let _g = RUNTIME_ENV.lock().unwrap();
        clear_runtime_token();
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        let err = verify_api(&c, &d, None, RUNTIME_TOKEN_ENV, None, true).unwrap_err();
        cleanup(&c);
        assert!(matches!(err, CliError::Config(_)));
        assert!(
            t.requests.lock().unwrap().is_empty(),
            "no calls should happen without the runtime token"
        );
    }
}
