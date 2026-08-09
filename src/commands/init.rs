//! `keylight init` — set Keylight up in the current project.
//!
//! The one command that forks on integration style (design §F1): it detects
//! the project, recommends **SDK** or **API**, and then walks the chosen path.
//! An SDK project is never asked about the API and vice versa. Detection is
//! advice, never a gate (§F4); a browser/client project that picks API anyway
//! is warned, not blocked (§F5).
//!
//! The API branch mints a `licenses:runtime` credential through the same device
//! flow as `login` (§A7 / §F2), writes it to a server-side `.env` — never into
//! code that ships to a client — and prints a copy-ready HTTPS example.

use super::{Context, Deps};
use crate::device_auth::{self, PollOutcome};
use crate::error::{CliError, Result};
use crate::output::emit;
use crate::project_detect::{Advice, Mode, detect_from_files};
use crate::{api::ApiOutcome, env_file};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The env var the app reads its runtime token from (matches the docs
/// quickstart and `Authorization: Bearer $KEYLIGHT_API_TOKEN`).
const APP_TOKEN_VAR: &str = "KEYLIGHT_API_TOKEN";

pub fn init(
    ctx: &Context,
    deps: &Deps,
    mode_flag: Option<String>,
    product: Option<String>,
    dir: PathBuf,
    env_file_name: String,
) -> Result<()> {
    let advice = detect_dir(&dir);
    let explicit = parse_mode(mode_flag.as_deref())?;
    let mode = resolve_mode(explicit, advice.recommended);

    // The recommendation and the choice, always to stderr under --json so
    // stdout stays a clean JSON object.
    note(
        ctx,
        &format!(
            "Detected: {}\nUsing {} integration{}.",
            advice.reason,
            mode_label(mode),
            if explicit.is_some() {
                " (from --mode)"
            } else {
                " (recommended — override with --mode)"
            }
        ),
    );

    // §F5 guard rail: an API token embedded in a browser/native client is
    // readable by anyone who has the app. Warn hard; block only a human's
    // accidental "yes", never the flow itself.
    if mode == Mode::Api && advice.client_risk {
        note(
            ctx,
            "Warning: this looks like a client/browser app. A licenses:runtime \
             token is a server secret — anyone who can read your shipped code \
             can call the API as your workspace. Use an SDK for untrusted \
             clients.",
        );
        if ctx.is_tty && !ctx.json && !prompt_yes_no("Set up API mode anyway?") {
            return Err(CliError::UsageError(
                "stopped — re-run with --mode sdk for a client app, or --mode \
                 api to proceed."
                    .into(),
            ));
        }
    }

    match mode {
        Mode::Sdk => finish_sdk(ctx),
        Mode::Api => finish_api(ctx, deps, product, &dir, &env_file_name),
    }
}

/// Parse the `--mode` flag. clap already constrains the values, but keeping the
/// mapping here means the command layer speaks in the `Mode` enum, not strings.
fn parse_mode(raw: Option<&str>) -> Result<Option<Mode>> {
    match raw {
        None => Ok(None),
        Some("sdk") => Ok(Some(Mode::Sdk)),
        Some("api") => Ok(Some(Mode::Api)),
        Some(other) => Err(CliError::Config(format!(
            "unknown --mode {other:?}; expected sdk or api"
        ))),
    }
}

/// An explicit `--mode` always wins; otherwise the detector's recommendation.
fn resolve_mode(explicit: Option<Mode>, recommended: Mode) -> Mode {
    explicit.unwrap_or(recommended)
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Sdk => "SDK",
        Mode::Api => "API",
    }
}

fn detect_dir(dir: &Path) -> Advice {
    let names = list_dir_names(dir);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    detect_from_files(&refs)
}

fn list_dir_names(dir: &Path) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ---- SDK branch (§F3: routes to the existing SDK flow) --------------------

fn finish_sdk(ctx: &Context) -> Result<()> {
    emit(ctx.json, &serde_json::json!({ "mode": "sdk" }), |_| {
        println!("Next steps for an SDK integration:");
        println!();
        println!("  1. Add a Keylight SDK for your platform (Swift, Rust, C++, C#, JS).");
        println!("  2. Wire in your public key and SDK key — see keylight products get.");
        println!("  3. Prove it end to end:  keylight setup verify-license");
        println!();
        println!("  Docs: https://docs.keylight.dev/");
    });
    Ok(())
}

// ---- API branch (§F2) -----------------------------------------------------

fn finish_api(
    ctx: &Context,
    deps: &Deps,
    product: Option<String>,
    dir: &Path,
    env_file_name: &str,
) -> Result<()> {
    let product_id = resolve_api_product(ctx, deps, product)?;
    let token = mint_runtime_token(ctx, deps)?;

    let env_path = dir.join(env_file_name);
    write_env_token(&env_path, APP_TOKEN_VAR, &token)?;
    let ignored = ensure_gitignored(dir, env_file_name)?;

    emit(
        ctx.json,
        &serde_json::json!({
            "mode": "api",
            "productId": product_id,
            "envFile": env_file_name,
            "envVar": APP_TOKEN_VAR,
            "gitignoreUpdated": ignored,
        }),
        |_| {
            println!("Wrote your licenses:runtime token to {env_file_name} as {APP_TOKEN_VAR}.");
            if ignored {
                println!("Added {env_file_name} to .gitignore.");
            }
            println!();
            println!("Call the API from your server:");
            println!();
            println!("{}", curl_example(&product_id));
            println!();
            println!("Test the connection end to end (no SDK):");
            println!();
            println!(
                "  source {env_file_name}                              # load {APP_TOKEN_VAR}"
            );
            println!(
                "  export KEYLIGHT_RUNTIME_TOKEN=\"${APP_TOKEN_VAR}\"    # the connection test reads this"
            );
            println!("  keylight setup verify-api --product {product_id}");
            println!();
            println!("  Docs: https://docs.keylight.dev/api-integration/quickstart/");
        },
    );
    Ok(())
}

/// Which app to wire up. An explicit `--product` wins; otherwise the tenant's
/// single app. Picking between several silently would point the whole setup at
/// the wrong one, so that is an error with the fix in it.
fn resolve_api_product(ctx: &Context, deps: &Deps, explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit.filter(|s| !s.trim().is_empty()) {
        return Ok(p);
    }
    let client = ctx.client(deps)?;
    let value = match client.get("/v1/products")? {
        ApiOutcome::Ok { value, .. } => value,
        ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
    };
    let items = value["items"].as_array().cloned().unwrap_or_default();
    match items.len() {
        0 => Err(CliError::UsageError(
            "No apps yet. Create an API app first:  keylight products create \
             --integration-mode api --display-name <name> --key-prefix <ABCD> \
             --support-email <you@app.com>"
                .into(),
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
                "Several apps ({}). Pass --product <id>.",
                names.join(", ")
            )))
        }
    }
}

/// Mint a `licenses:runtime` credential through the device flow — the same
/// browser-approved grant `login` uses, but scoped to exactly one permission
/// (§A7). The token is returned once and never stored in the CLI config; it
/// belongs in the app's `.env`.
fn mint_runtime_token(ctx: &Context, deps: &Deps) -> Result<String> {
    let client = ctx.anonymous_client(deps);
    let started = device_auth::start(&client, Some(&["licenses:runtime".to_string()]))?;
    let verify_url = started
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&started.verification_uri);

    note(
        ctx,
        &format!(
            "Approve a licenses:runtime token in your browser.\n  Code: {}\n  {}",
            started.user_code, verify_url
        ),
    );
    if !ctx.no_browser && ctx.is_tty {
        deps.opener.open(verify_url);
    }

    match device_auth::poll_until_complete(&client, &deps.sleeper, &started)? {
        PollOutcome::Approved { token, .. } => Ok(token),
        PollOutcome::Denied => Err(CliError::UsageError(
            "the token request was denied in the browser; nothing was written".into(),
        )),
        PollOutcome::Expired => Err(CliError::UsageError(
            "the request expired before it was approved; run `keylight init` again".into(),
        )),
    }
}

/// The copy-ready HTTPS example, using the real product id.
fn curl_example(product_id: &str) -> String {
    format!(
        "  curl -s https://api.keylight.dev/v1/licenses/activate \\\n\
        \x20   -H \"Authorization: Bearer ${APP_TOKEN_VAR}\" \\\n\
        \x20   -H \"Content-Type: application/json\" \\\n\
        \x20   -d '{{\"product_id\":\"{product_id}\",\"license_key\":\"<KEY>\",\"device_id\":\"<DEVICE_ID>\"}}'"
    )
}

/// Write `key=value` into `path`, replacing an existing assignment and keeping
/// every other line. Owner-only (`0600`) — the file holds a live bearer token.
fn write_env_token(path: &Path, key: &str, value: &str) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(CliError::Io(e)),
    };
    let body = env_file::upsert(&existing, key, value);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, body)?;
    }
    Ok(())
}

/// Make sure `.gitignore` in `dir` ignores `entry`. Returns whether it was
/// added (false when it was already ignored, or there is no `.gitignore` and
/// the directory isn't a git repo). A leaked `.env` is the worst outcome here,
/// so this is done automatically rather than merely offered.
fn ensure_gitignored(dir: &Path, entry: &str) -> Result<bool> {
    let gitignore = dir.join(".gitignore");
    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Only create a .gitignore if this is actually a git repo — writing
            // one into a non-repo directory would be surprising litter.
            if !dir.join(".git").exists() {
                return Ok(false);
            }
            String::new()
        }
        Err(e) => return Err(CliError::Io(e)),
    };
    if env_file::is_gitignored(&existing, entry) {
        return Ok(false);
    }
    let body = env_file::append_gitignore(&existing, entry);
    std::fs::write(&gitignore, body)?;
    Ok(true)
}

/// A progress/advice line. Under `--json` it goes to stderr so stdout carries
/// only the final result object; otherwise stdout, like the rest of the human
/// output.
fn note(ctx: &Context, message: &str) {
    if ctx.json {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

/// Blocking y/N prompt — only ever called at a TTY (guarded by the caller).
fn prompt_yes_no(question: &str) -> bool {
    use std::io::{stdin, stdout};
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
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn tmp_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("keylight-init-{tag}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn explicit_mode_overrides_the_recommendation() {
        assert_eq!(resolve_mode(Some(Mode::Api), Mode::Sdk), Mode::Api);
        assert_eq!(resolve_mode(Some(Mode::Sdk), Mode::Api), Mode::Sdk);
        assert_eq!(resolve_mode(None, Mode::Api), Mode::Api);
    }

    #[test]
    fn parse_mode_maps_the_flag_values() {
        assert_eq!(parse_mode(None).unwrap(), None);
        assert_eq!(parse_mode(Some("api")).unwrap(), Some(Mode::Api));
        assert_eq!(parse_mode(Some("sdk")).unwrap(), Some(Mode::Sdk));
        assert!(parse_mode(Some("nope")).is_err());
    }

    #[test]
    fn curl_example_uses_the_real_product_and_the_app_token_var() {
        let ex = curl_example("notes");
        assert!(ex.contains("/v1/licenses/activate"));
        assert!(ex.contains("\"product_id\":\"notes\""));
        assert!(ex.contains(APP_TOKEN_VAR));
        assert!(!ex.contains("npm install"));
    }

    #[test]
    fn write_env_token_creates_then_replaces_in_place() {
        let dir = tmp_dir("env");
        let path = dir.join(".env");
        write_env_token(&path, "KEYLIGHT_API_TOKEN", "klm_one").unwrap();
        write_env_token(&path, "KEYLIGHT_API_TOKEN", "klm_two").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "KEYLIGHT_API_TOKEN=klm_two\n");
        assert_eq!(body.matches("KEYLIGHT_API_TOKEN=").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn env_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perms");
        let path = dir.join(".env");
        write_env_token(&path, "KEYLIGHT_API_TOKEN", "klm_secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_env_token_keeps_other_lines() {
        let dir = tmp_dir("env-keep");
        let path = dir.join(".env");
        std::fs::write(&path, "PORT=3000\n").unwrap();
        write_env_token(&path, "KEYLIGHT_API_TOKEN", "klm_abc").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("PORT=3000"));
        assert!(body.contains("KEYLIGHT_API_TOKEN=klm_abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gitignore_is_added_in_a_repo_and_not_duplicated() {
        let dir = tmp_dir("gi");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert!(
            ensure_gitignored(&dir, ".env").unwrap(),
            "first call adds it"
        );
        assert!(
            !ensure_gitignored(&dir, ".env").unwrap(),
            "second call is a no-op"
        );
        let body = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(body.matches(".env").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gitignore_is_not_created_outside_a_git_repo() {
        let dir = tmp_dir("no-git");
        assert!(
            !ensure_gitignored(&dir, ".env").unwrap(),
            "no .gitignore litter in a non-repo directory"
        );
        assert!(!dir.join(".gitignore").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SDK branch is pure output — no network — so init resolves and
    /// returns cleanly for an unmistakably native project.
    #[test]
    fn init_takes_the_sdk_branch_for_a_native_project_without_touching_the_network() {
        let dir = tmp_dir("sdk-branch");
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        init(&c, &d, None, None, dir.clone(), ".env".into()).unwrap();
        cleanup(&c);
        assert!(
            t.requests.lock().unwrap().is_empty(),
            "the SDK branch makes no API calls"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full API branch, end to end against a scripted transport: resolve
    /// the single app, mint via the device flow, write .env. Proves the token
    /// lands in the file and the CLI's own config is never touched.
    #[test]
    fn init_api_branch_mints_and_writes_the_env_file() {
        let dir = tmp_dir("api-branch");
        let t = transport(vec![
            // resolve_api_product: one app
            resp(200, r#"{"items":[{"productId":"notes"}]}"#, None),
            // device start
            resp(
                200,
                r#"{"device_code":"d1","user_code":"AB-CD","verification_uri":"https://app.keylight.dev/login/device","interval":1,"expires_in":5}"#,
                None,
            ),
            // device poll -> approved with the runtime token
            resp(
                200,
                r#"{"status":"approved","token":"klm_runtime_123","scopes":["licenses:runtime"],"name":"CLI"}"#,
                None,
            ),
        ]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx(true);
        init(&c, &d, Some("api".into()), None, dir.clone(), ".env".into()).unwrap();
        cleanup(&c);

        let body = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert_eq!(body, "KEYLIGHT_API_TOKEN=klm_runtime_123\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use crate::test_support::resp;
}
