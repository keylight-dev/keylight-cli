//! Auto step-up for destructive commands: when the API rejects a call with
//! `403 Missing scope: danger`, re-authorize for `danger` through the existing
//! device flow, then let the caller retry. npm-style: page 1 grants the
//! capability, page 2 (the existing confirmation) approves the specific action.

use super::{Context, Deps};
use crate::api::ApiOutcome;
use crate::config::{self, ConfigFile};
use crate::device_auth::{self, PollOutcome};
use crate::error::{CliError, Result};

/// The token's current scopes with `danger` appended when missing.
///
/// The device flow treats an explicit scope list as *exact*, so re-authorizing
/// for `danger` alone would silently drop every capability the token already
/// had. We request the union instead. Order is preserved so the grant page
/// lists permissions in the same order the user last saw them.
fn scopes_with_danger(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|s| s == "danger") {
        out.push("danger".to_string());
    }
    out
}

/// Re-authorize the CLI for `danger` (browser page 1). On success the config
/// file holds a fresh token carrying `danger`; the caller rebuilds its client
/// and retries the original call, which then reaches the normal confirmation
/// flow (page 2).
pub fn grant_danger(ctx: &Context, deps: &Deps) -> Result<()> {
    // A token from the environment wins over the config file, so re-authorizing
    // into the config would have no effect — fail with the real remedy instead
    // of silently looping into a second 403.
    if ctx.env_token().is_some() {
        return Err(CliError::Config(
            "your token comes from KEYLIGHT_API_TOKEN, which overrides the stored login; \
             reissue that token with the danger scope, or unset it and run `keylight login`"
                .into(),
        ));
    }

    // Current scopes, so the re-auth requests the union rather than downgrading.
    let client = ctx.client(deps)?;
    let verify = match client.get("/v1/tokens/verify")? {
        ApiOutcome::Ok { value, .. } => value,
        ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
    };
    let current: Vec<String> = verify
        .get("scopes")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let requested = scopes_with_danger(&current);

    // Page 1: the existing device-flow grant page, now showing the danger toggle.
    let anon = ctx.anonymous_client(deps);
    let started = device_auth::start(&anon, Some(&requested))?;
    let verify_url = started
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&started.verification_uri);

    eprintln!(
        "This is a destructive action needing the \"destructive actions\" permission your login doesn't have yet."
    );
    eprintln!("Approve it — and turn on the \"Destructive actions\" toggle — at: {verify_url}");
    eprintln!("Your code: {}", started.user_code);
    if !ctx.no_browser && ctx.is_tty {
        deps.opener.open(verify_url);
    }

    match device_auth::poll_until_complete(&anon, &deps.sleeper, &started)? {
        PollOutcome::Approved { token, scopes, .. } => {
            if !scopes.iter().any(|s| s == "danger") {
                return Err(CliError::Config(
                    "Destructive actions were not enabled — re-run and turn on the \
                     \"Destructive actions\" toggle to continue."
                        .into(),
                ));
            }
            config::write_config(&ctx.config_path, &ConfigFile { token: Some(token) })?;
            eprintln!("Permission granted.");
            Ok(())
        }
        PollOutcome::Denied => Err(CliError::Config(
            "permission was denied in the browser; nothing changed".into(),
        )),
        PollOutcome::Expired => Err(CliError::Config(
            "the approval code expired before it was approved; run the command again".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Deps;
    use crate::confirmation::Opener;
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn appends_danger_when_absent() {
        let got = scopes_with_danger(&["licenses:read".to_string(), "licenses:write".to_string()]);
        assert_eq!(got, vec!["licenses:read", "licenses:write", "danger"]);
    }

    #[test]
    fn leaves_scopes_untouched_when_danger_already_present() {
        let got = scopes_with_danger(&["licenses:write".to_string(), "danger".to_string()]);
        assert_eq!(got, vec!["licenses:write", "danger"]);
    }

    #[test]
    fn appends_danger_to_an_empty_scope_set() {
        let got = scopes_with_danger(&[]);
        assert_eq!(got, vec!["danger"]);
    }

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct RecordingOpener {
        opened: Mutex<Vec<String>>,
    }
    impl Opener for RecordingOpener {
        fn open(&self, url: &str) {
            self.opened.lock().unwrap().push(url.to_string());
        }
    }

    const VERIFY_BODY: &str =
        r#"{"tenantId":"t1","tokenId":"tok1","scopes":["licenses:read","licenses:write"]}"#;
    const START_BODY: &str = r#"{"device_code":"dev1","user_code":"ABCD-1234","verification_uri":"https://app.keylight.dev/login/device","verification_uri_complete":"https://app.keylight.dev/login/device?code=ABCD-1234","interval":5,"expires_in":900}"#;
    const APPROVED_WITH_DANGER: &str = r#"{"status":"approved","token":"klm_danger","scopes":["licenses:read","licenses:write","danger"],"name":"laptop"}"#;
    const APPROVED_WITHOUT_DANGER: &str = r#"{"status":"approved","token":"klm_plain","scopes":["licenses:read","licenses:write"],"name":"laptop"}"#;

    fn stepup_ctx(no_browser: bool, is_tty: bool) -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("keylight-cli-stepup-{n}.toml"));
        // A pre-existing token so `ctx.client()` can make the verify call.
        config::write_config(
            &path,
            &ConfigFile {
                token: Some("klm_old".into()),
            },
        )
        .unwrap();
        Context {
            json: true,
            no_browser,
            api_url: "https://api.test".into(),
            config_path: path,
            is_tty,
        }
    }

    fn deps_with<'a>(transport: Arc<ScriptedTransport>, opener: &'a RecordingOpener) -> Deps<'a> {
        Deps {
            transport,
            sleeper: Arc::new(NoopSleeper::default()),
            opener,
            max_polls: Some(5),
        }
    }

    #[test]
    fn grant_danger_happy_path_persists_new_token_and_opens_browser() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            resp(200, VERIFY_BODY, None),
            resp(200, START_BODY, None),
            resp(200, APPROVED_WITH_DANGER, None),
        ]));
        let opener = RecordingOpener {
            opened: Mutex::new(Vec::new()),
        };
        let deps = deps_with(transport.clone(), &opener);
        let ctx = stepup_ctx(false, true);

        grant_danger(&ctx, &deps).unwrap();

        // New token persisted.
        let cfg = config::read_config(&ctx.config_path).unwrap();
        assert_eq!(cfg.token.as_deref(), Some("klm_danger"));
        // Browser opened on the complete verification URL.
        assert_eq!(
            opener.opened.lock().unwrap().as_slice(),
            ["https://app.keylight.dev/login/device?code=ABCD-1234"]
        );
        // Device start requested the union (danger appended to current scopes).
        let reqs = transport.requests.lock().unwrap();
        let start = reqs
            .iter()
            .find(|r| r.1.ends_with("/v1/login/device"))
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(start.3.as_deref().unwrap()).unwrap();
        assert_eq!(
            body["scopes"],
            serde_json::json!(["licenses:read", "licenses:write", "danger"])
        );

        let _ = config::delete_config(&ctx.config_path);
    }

    #[test]
    fn grant_danger_errors_when_toggle_left_off_and_does_not_persist() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            resp(200, VERIFY_BODY, None),
            resp(200, START_BODY, None),
            resp(200, APPROVED_WITHOUT_DANGER, None),
        ]));
        let opener = RecordingOpener {
            opened: Mutex::new(Vec::new()),
        };
        let deps = deps_with(transport, &opener);
        let ctx = stepup_ctx(true, false);

        let err = grant_danger(&ctx, &deps).unwrap_err();
        assert!(matches!(err, CliError::Config(m) if m.contains("Destructive actions")));
        // Old token untouched.
        let cfg = config::read_config(&ctx.config_path).unwrap();
        assert_eq!(cfg.token.as_deref(), Some("klm_old"));

        let _ = config::delete_config(&ctx.config_path);
    }

    // `KEYLIGHT_API_TOKEN` is process-global: setting it here raced other
    // tests in this binary (in other modules, not just this one) that build a
    // `Context` and assume the env var is unset, occasionally making them
    // resolve `klm_env` instead of their own file-based token. Confirmed via
    // repeated `cargo test --lib commands::stepup::tests` runs — passed under
    // `--test-threads=1`, intermittently failed at default parallelism. Run
    // with `cargo test -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn grant_danger_refuses_an_env_supplied_token() {
        // Guarded before any network call: with the env token set, writing the
        // config file cannot change which token ctx.client() resolves.
        let transport = Arc::new(ScriptedTransport::new(vec![]));
        let opener = RecordingOpener {
            opened: Mutex::new(Vec::new()),
        };
        let deps = deps_with(transport.clone(), &opener);
        let ctx = stepup_ctx(true, false);

        // SAFETY: single-threaded within this test; restored before returning.
        unsafe { std::env::set_var(config::ENV_TOKEN, "klm_env") };
        let err = grant_danger(&ctx, &deps).unwrap_err();
        unsafe { std::env::remove_var(config::ENV_TOKEN) };

        assert!(matches!(err, CliError::Config(m) if m.contains("KEYLIGHT_API_TOKEN")));
        assert!(
            transport.requests.lock().unwrap().is_empty(),
            "must not hit the network"
        );

        let _ = config::delete_config(&ctx.config_path);
    }
}
