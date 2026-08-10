pub mod auth;
pub mod customers;
pub mod init;
pub mod integrations;
pub mod licenses;
pub mod login;
pub mod logout;
pub mod misc;
pub mod products;
pub mod setup;
pub mod stepup;

use crate::api::ApiClient;
use crate::config;
use crate::confirmation::{
    ConfirmOptions, Opener, RealOpener, run_confirmable, run_confirmable_raw,
};
use crate::error::Result;
use crate::http::Transport;
use crate::http::ureq_transport::UreqTransport;
use crate::sleep::{RealSleeper, Sleeper};
use std::path::PathBuf;
use std::sync::Arc;

/// Everything a command needs, resolved once in `main` from CLI flags + env.
pub struct Context {
    pub json: bool,
    pub no_browser: bool,
    pub api_url: String,
    pub config_path: PathBuf,
    /// True when stdout is an interactive terminal — gates browser-opening
    /// (never pop a browser in a CI log or a piped script) alongside the
    /// explicit `--no-browser` flag.
    ///
    /// A resolved field rather than a method reading the real stdout, so a
    /// test can exercise both sides of the gate. As a method it was
    /// unconditionally `false` under `cargo test`, which meant the
    /// browser-opening branch had no coverage at all.
    pub is_tty: bool,
}

impl Context {
    pub fn env_token(&self) -> Option<String> {
        std::env::var(config::ENV_TOKEN).ok()
    }

    /// An authenticated client, or `CliError::NoToken`. Every command needs
    /// exactly this, and repeating the construction inline is what made the
    /// TTY/browser gate untestable in `login` — the transport was baked in at
    /// the call site with no seam to substitute.
    pub fn client(&self, deps: &Deps) -> Result<ApiClient> {
        let token = config::require_token(self.env_token(), &self.config_path)?;
        Ok(ApiClient::new(
            deps.transport.clone(),
            deps.sleeper.clone(),
            self.api_url.clone(),
            token,
        ))
    }

    /// A client for the two endpoints that take no auth — the device-login
    /// start/poll pair.
    pub fn anonymous_client(&self, deps: &Deps) -> ApiClient {
        ApiClient::anonymous(
            deps.transport.clone(),
            deps.sleeper.clone(),
            self.api_url.clone(),
        )
    }
}

/// The real browser opener. A unit struct, so it can live in a `static` and
/// be borrowed by [`Deps::real`] without an allocation or a lifetime knot.
pub static REAL_OPENER: RealOpener = RealOpener;

/// The side-effecting collaborators a command needs: HTTP, sleeping, and
/// opening a browser. Injected rather than constructed in place so that the
/// paths which are otherwise untestable — the TTY/no-browser gate, the
/// confirm-gated poll loop — can be driven from a test.
pub struct Deps<'a> {
    pub transport: Arc<dyn Transport>,
    pub sleeper: Arc<dyn Sleeper>,
    pub opener: &'a dyn Opener,
    /// Safety valve for the confirmation poll loop; `None` in production,
    /// where the server-side confirmation expires on its own.
    pub max_polls: Option<u32>,
}

impl Deps<'static> {
    /// The production wiring: real HTTP, real sleeping, real browser.
    pub fn real() -> Self {
        Deps {
            transport: Arc::new(UreqTransport::default()),
            sleeper: Arc::new(RealSleeper),
            opener: &REAL_OPENER,
            max_polls: None,
        }
    }
}

/// Render the "this needs approval" notice.
///
/// Always stderr, in both output modes. Under `--json` stdout must carry only
/// the result object; and `keylight licenses export > licenses.csv` must put
/// CSV in the file, not an approval URL. Progress is not a result.
fn on_pending_notice(pending: &crate::api::PendingConfirmation) {
    let minutes = pending.expires_in_seconds / 60;
    eprintln!("This operation needs approval in your browser.");
    eprintln!("Approve it at: {}", pending.approve_url);
    eprintln!("Waiting for approval (expires in {minutes} min, Ctrl+C to cancel)...");
}

fn confirm_opts<'a>(ctx: &Context, deps: &'a Deps<'a>) -> ConfirmOptions<'a> {
    ConfirmOptions {
        no_browser: ctx.no_browser,
        is_tty: ctx.is_tty,
        opener: deps.opener,
        sleeper: deps.sleeper.as_ref(),
        on_pending: &on_pending_notice,
        max_polls: deps.max_polls,
    }
}

/// A `403` whose body says exactly `Missing scope: danger` — the one condition
/// the auto step-up heals. Any other missing scope is a real authorization gap
/// the user must resolve by re-logging with the right resource scopes.
fn is_missing_danger(err: &crate::error::CliError) -> bool {
    matches!(
        err,
        crate::error::CliError::Api { status: 403, message } if message == "Missing scope: danger"
    )
}

/// Run a call that may be confirm-gated, walking the approval flow if the
/// server answers `202`. Ungated endpoints go through unchanged, so a command
/// never has to know which tier it is calling.
///
/// A `403 Missing scope: danger` triggers one auto step-up: grant the danger
/// scope through the existing device flow, rebuild the client from the
/// freshly stored token, and retry once. Any other error — including any
/// other missing scope — surfaces unchanged.
pub fn run_gated(
    ctx: &Context,
    deps: &Deps,
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    match run_confirmable(client, method, path, body, &confirm_opts(ctx, deps)) {
        Err(e) if is_missing_danger(&e) => {
            stepup::grant_danger(ctx, deps)?;
            let client = ctx.client(deps)?; // rebuilt from the freshly stored token
            run_confirmable(&client, method, path, body, &confirm_opts(ctx, deps))
        }
        other => other,
    }
}

/// As [`run_gated`], but returns the success body verbatim. Only
/// `licenses export` needs it — the response is `text/csv`.
pub fn run_gated_raw(
    ctx: &Context,
    deps: &Deps,
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<String> {
    match run_confirmable_raw(client, method, path, body, &confirm_opts(ctx, deps)) {
        Err(e) if is_missing_danger(&e) => {
            stepup::grant_danger(ctx, deps)?;
            let client = ctx.client(deps)?;
            run_confirmable_raw(&client, method, path, body, &confirm_opts(ctx, deps))
        }
        other => other,
    }
}

/// Append the query parameters that were actually supplied.
///
/// Values are percent-encoded: a pagination cursor is opaque server state and
/// routinely contains `=`, `+` or `/`, any of which would be misread as
/// structure if pasted in raw.
pub fn query_path(base: &str, params: &[(&str, Option<String>)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in params {
        if let Some(v) = v {
            parts.push(format!("{k}={}", percent_encode(v)));
        }
    }
    if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parts.join("&"))
    }
}

/// Percent-encode everything outside the unreserved set (RFC 3986 §2.3).
/// Small enough not to justify a dependency, and conservative enough that
/// over-encoding is never wrong.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Resolve a mutually exclusive on/off flag pair into a bool.
///
/// Neither flag is an error rather than a default. For an operation that
/// writes a boolean to a live app, guessing which way the user meant is the
/// one behavior with no safe answer.
pub fn require_toggle(on: bool, off: bool, on_flag: &str, off_flag: &str) -> Result<bool> {
    match (on, off) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        (false, false) => Err(crate::error::CliError::Config(format!(
            "pass {on_flag} or {off_flag}"
        ))),
        (true, true) => Err(crate::error::CliError::Config(format!(
            "{on_flag} and {off_flag} are mutually exclusive"
        ))),
    }
}

/// Read a secret from the environment.
///
/// Secrets never arrive as flags — argv lands in shell history and is visible
/// to other users via `ps`. The flag names the *variable*, so the value stays
/// out of the command line while every input remains available
/// non-interactively (no command may require a prompt).
pub fn secret_from_env(var: &str) -> Result<String> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(crate::error::CliError::Config(format!(
            "set {var} to the secret value (it is read from the environment, \
             never from a flag, so it stays out of shell history)"
        ))),
    }
}

/// Parse a flag whose value is a JSON document, reporting which flag failed.
/// `serde_json`'s own message alone ("expected value at line 1 column 1")
/// doesn't say which of several JSON-valued flags was malformed.
pub fn parse_json_flag(raw: Option<String>, flag: &str) -> Result<Option<serde_json::Value>> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| crate::error::CliError::Config(format!("{flag} is not valid JSON: {e}"))),
    }
}

/// Build a JSON object from only the fields that were actually supplied.
///
/// `PATCH` endpoints treat a present key as "set it" — sending `null` for an
/// unsupplied flag would clear a field the user never mentioned. Every
/// optional flag goes through here.
pub fn json_body(fields: Vec<(&str, Option<serde_json::Value>)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in fields {
        if let Some(v) = v {
            map.insert(k.to_string(), v);
        }
    }
    serde_json::Value::Object(map)
}

/// Shared scaffolding for command tests: a `Context` backed by a throwaway
/// config file holding a token, and `Deps` wired to a scripted transport.
#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::config::{self, ConfigFile};
    use crate::confirmation::Opener;
    use crate::http::TransportOutcome;
    use crate::sleep::NoopSleeper;
    use crate::test_support::ScriptedTransport;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    pub struct RecordingOpener {
        pub opened: Mutex<Vec<String>>,
    }
    impl Default for RecordingOpener {
        fn default() -> Self {
            Self {
                opened: Mutex::new(Vec::new()),
            }
        }
    }
    impl RecordingOpener {
        pub fn new() -> Self {
            Self::default()
        }
    }
    impl Opener for RecordingOpener {
        fn open(&self, url: &str) {
            self.opened.lock().unwrap().push(url.to_string());
        }
    }

    /// A context whose config file carries a token, so `ctx.client()`
    /// resolves without touching the developer's real config.
    pub fn ctx(json: bool) -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("keylight-cli-cmd-test-{n}.toml"));
        config::write_config(
            &path,
            &ConfigFile {
                token: Some("klm_test".into()),
            },
        )
        .unwrap();
        Context {
            json,
            no_browser: true,
            api_url: "https://api.test".into(),
            config_path: path,
            is_tty: false,
        }
    }

    pub fn cleanup(ctx: &Context) {
        let _ = config::delete_config(&ctx.config_path);
    }

    pub fn deps<'a>(transport: &Arc<ScriptedTransport>, opener: &'a RecordingOpener) -> Deps<'a> {
        Deps {
            transport: transport.clone(),
            sleeper: Arc::new(NoopSleeper::default()),
            opener,
            max_polls: Some(10),
        }
    }

    pub fn transport(responses: Vec<TransportOutcome>) -> Arc<ScriptedTransport> {
        Arc::new(ScriptedTransport::new(responses))
    }

    /// The (method, url, body) of the nth recorded request.
    pub fn request_at(t: &Arc<ScriptedTransport>, n: usize) -> (String, String, Option<String>) {
        let reqs = t.requests.lock().unwrap();
        let (m, u, _, b) = &reqs[n];
        (m.clone(), u.clone(), b.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_path_with_no_params_is_the_bare_path() {
        assert_eq!(
            query_path("/v1/products", &[("limit", None)]),
            "/v1/products"
        );
    }

    /// A cursor is opaque server state and routinely contains `=` or `+`;
    /// pasting it in raw would corrupt the query string.
    #[test]
    fn query_values_are_percent_encoded() {
        let p = query_path("/v1/products", &[("cursor", Some("a+b/c=".into()))]);
        assert_eq!(p, "/v1/products?cursor=a%2Bb%2Fc%3D");
    }

    #[test]
    fn query_path_joins_several_params() {
        let p = query_path(
            "/v1/licenses",
            &[
                ("limit", Some("10".into())),
                ("status", Some("active".into())),
            ],
        );
        assert_eq!(p, "/v1/licenses?limit=10&status=active");
    }

    /// Guessing which way an on/off flag was meant has no safe answer when
    /// the write lands on a live app.
    #[test]
    fn a_toggle_with_neither_flag_is_an_error_not_a_default() {
        assert!(require_toggle(false, false, "--enabled", "--disabled").is_err());
    }

    #[test]
    fn a_toggle_resolves_each_direction() {
        assert!(require_toggle(true, false, "--enabled", "--disabled").unwrap());
        assert!(!require_toggle(false, true, "--enabled", "--disabled").unwrap());
    }

    /// The error names the flag, since several flags take JSON and
    /// serde's own message doesn't say which one failed.
    #[test]
    fn a_malformed_json_flag_names_the_flag_that_failed() {
        let err = parse_json_flag(Some("{not json".into()), "--prices").unwrap_err();
        assert!(err.to_string().contains("--prices"), "got: {err}");
    }

    #[test]
    fn json_body_omits_unsupplied_fields_entirely() {
        let body = json_body(vec![
            ("supportEmail", Some(serde_json::json!("a@b.com"))),
            ("purchaseURL", None),
        ]);
        assert_eq!(body, serde_json::json!({"supportEmail": "a@b.com"}));
        assert!(
            !body.as_object().unwrap().contains_key("purchaseURL"),
            "an unsupplied field must be absent, not null — PATCH treats a \
             present null as an instruction to clear the field"
        );
    }

    /// An explicit null is different from an unsupplied field: `webhook set
    /// --url ""` clears the webhook, and that must survive to the wire.
    #[test]
    fn json_body_keeps_an_explicit_null() {
        let body = json_body(vec![("url", Some(serde_json::Value::Null))]);
        assert_eq!(body, serde_json::json!({ "url": null }));
    }

    #[test]
    fn json_body_with_nothing_supplied_is_an_empty_object() {
        let body = json_body(vec![("a", None), ("b", None)]);
        assert_eq!(body, serde_json::json!({}));
    }
}

#[cfg(test)]
mod run_gated_tests {
    use super::*;
    use crate::confirmation::Opener;
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct NoopOpener;
    impl Opener for NoopOpener {
        fn open(&self, _url: &str) {}
    }

    fn ctx_with_token() -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("keylight-cli-rungated-{n}.toml"));
        config::write_config(
            &path,
            &config::ConfigFile {
                token: Some("klm_old".into()),
            },
        )
        .unwrap();
        Context {
            json: true,
            no_browser: true,
            api_url: "https://api.test".into(),
            config_path: path,
            is_tty: false,
        }
    }

    // 403 danger -> step-up (verify, device start, approved w/ danger) ->
    // retry -> 202 confirmation -> poll approved -> final 200.
    #[test]
    fn missing_danger_triggers_stepup_then_retries_to_success() {
        static OPENER: NoopOpener = NoopOpener;
        let transport = Arc::new(ScriptedTransport::new(vec![
            resp(403, r#"{"error":"Missing scope: danger"}"#, None), // initial revoke
            resp(
                200,
                r#"{"tenantId":"t1","tokenId":"tok1","scopes":["licenses:write"]}"#,
                None,
            ), // verify
            resp(
                200,
                r#"{"device_code":"d","user_code":"AB-12","verification_uri":"https://app.keylight.dev/login/device","verification_uri_complete":"https://app.keylight.dev/login/device?code=AB-12","interval":5,"expires_in":900}"#,
                None,
            ), // device start
            resp(
                200,
                r#"{"status":"approved","token":"klm_danger","scopes":["licenses:write","danger"],"name":"l"}"#,
                None,
            ), // poll approved
            resp(
                202,
                r#"{"confirmation":{"id":"c1","status":"pending","approveUrl":"https://app.keylight.dev/c/c1","expiresInSeconds":600}}"#,
                None,
            ), // retried revoke: gated
            resp(200, r#"{"status":"approved"}"#, Some(0)),          // confirmation poll
            resp(200, r#"{"revoked":true}"#, None),                  // confirmed revoke
        ]));
        let deps = Deps {
            transport: transport.clone(),
            sleeper: Arc::new(NoopSleeper::default()),
            opener: &OPENER,
            max_polls: Some(10),
        };
        let ctx = ctx_with_token();
        let client = ctx.client(&deps).unwrap();

        let out = run_gated(&ctx, &deps, &client, "POST", "/v1/licenses/L1/revoke", None).unwrap();
        assert_eq!(out["revoked"], true);

        // The retried revoke — the final recorded request — must carry the
        // freshly granted token, not the pre-step-up one: step-up rebuilds
        // the client from the stored config, and a stale token here would
        // mean the retry never actually picked up the new grant.
        let reqs = transport.requests.lock().unwrap();
        let (_, _, last_headers, _) = reqs.last().unwrap();
        assert!(
            last_headers.contains(&("Authorization".to_string(), "Bearer klm_danger".to_string())),
            "expected the retried request to carry the new token, got headers: {last_headers:?}"
        );
        drop(reqs);

        let _ = config::delete_config(&ctx.config_path);
    }

    // A non-danger missing scope is surfaced unchanged: no step-up attempted.
    #[test]
    fn other_missing_scope_is_not_auto_healed() {
        static OPENER: NoopOpener = NoopOpener;
        let transport = Arc::new(ScriptedTransport::new(vec![resp(
            403,
            r#"{"error":"Missing scope: licenses:write"}"#,
            None,
        )]));
        let deps = Deps {
            transport: transport.clone(),
            sleeper: Arc::new(NoopSleeper::default()),
            opener: &OPENER,
            max_polls: Some(10),
        };
        let ctx = ctx_with_token();
        let client = ctx.client(&deps).unwrap();

        let err =
            run_gated(&ctx, &deps, &client, "POST", "/v1/licenses/L1/revoke", None).unwrap_err();
        assert!(matches!(
            err,
            crate::error::CliError::Api { status: 403, .. }
        ));
        // Only the one call was made — no verify/device-start followed.
        assert_eq!(transport.requests.lock().unwrap().len(), 1);

        let _ = config::delete_config(&ctx.config_path);
    }
}
