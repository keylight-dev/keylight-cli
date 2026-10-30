//! `keylight login` — RFC 8628 device authorization.
//!
//! Device flow rather than a loopback callback because the CLI has to work on
//! headless boxes and over SSH, where a browser cannot reach the CLI's
//! localhost. The human step happens on the server's grant page; this module
//! only starts the flow, shows the code, and waits.

use super::{Context, Deps};
use crate::config::{self, ConfigFile};
use crate::device_auth::{self, PollOutcome};
use crate::error::{CliError, Result};
use crate::output::emit;
use std::io::Write;

/// Print the code/URL/waiting-message progress lines.
///
/// Under `--json`, these go to `err` (stderr) rather than `out` (stdout), so
/// stdout carries only the final JSON object `emit` writes — a caller piping
/// stdout through `jq` must not see anything else. The URL is printed in
/// both modes: a non-interactive caller still needs it to complete the flow,
/// since the browser isn't auto-opened for it either.
fn print_progress(
    json: bool,
    user_code: &str,
    verify_url: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let w: &mut dyn Write = if json { err } else { out };
    let _ = writeln!(w, "Your code: {user_code}");
    let _ = writeln!(w, "Approve it at: {verify_url}");
    let _ = writeln!(w, "Waiting for approval (Ctrl+C to cancel)...");
}

pub fn login(ctx: &Context, deps: &Deps, scopes: Option<Vec<String>>) -> Result<()> {
    let client = ctx.anonymous_client(deps);
    let started = device_auth::start(&client, scopes.as_deref())?;

    let verify_url = started
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&started.verification_uri);

    print_progress(
        ctx.json,
        &started.user_code,
        verify_url,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );

    if !ctx.no_browser && ctx.is_tty {
        deps.opener.open(verify_url);
    }

    match device_auth::poll_until_complete(&client, &deps.sleeper, &started)? {
        PollOutcome::Approved {
            token,
            scopes,
            name,
        } => {
            config::write_config(&ctx.config_path, &ConfigFile { token: Some(token) })?;
            let result = serde_json::json!({
                "authenticated": true,
                "name": name,
                "scopes": scopes,
            });
            emit(ctx.json, &result, |_| {
                println!("Logged in. Token stored at {}.", ctx.config_path.display());
            });
            Ok(())
        }
        PollOutcome::Denied => Err(CliError::Config(
            "login was denied in the browser; nothing was stored".into(),
        )),
        PollOutcome::Expired => Err(CliError::Config(
            "the login code expired before it was approved; run `keylight login` again".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under `--json`, stdout must carry only the final JSON object emitted by
    // `emit` — a caller piping stdout through `jq` must not see the code/URL
    // progress lines. They still need to go *somewhere* non-interactively, so
    // they move to stderr rather than disappearing.
    #[test]
    fn json_mode_sends_progress_lines_to_stderr_not_stdout() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        print_progress(
            true,
            "ABCD-1234",
            "https://app.keylight.dev/x",
            &mut out,
            &mut err,
        );

        assert!(
            out.is_empty(),
            "stdout must carry only the final JSON object under --json, got {out:?}"
        );
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("ABCD-1234"));
        assert!(err_str.contains("https://app.keylight.dev/x"));
    }

    // The URL must still be printed in non-JSON mode too — a non-TTY caller
    // (no browser auto-open) needs it to complete the flow.
    #[test]
    fn interactive_mode_sends_progress_lines_to_stdout() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        print_progress(
            false,
            "ABCD-1234",
            "https://app.keylight.dev/x",
            &mut out,
            &mut err,
        );

        assert!(
            err.is_empty(),
            "nothing should go to stderr in interactive mode"
        );
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("ABCD-1234"));
        assert!(out_str.contains("https://app.keylight.dev/x"));
    }

    // ---- the TTY / --no-browser gate -------------------------------------
    //
    // This gate had no coverage until `Deps` and `Context::is_tty` became
    // injectable: `login` constructed RealOpener/UreqTransport inline, and
    // `is_tty()` read the real stdout, which is never a terminal under
    // `cargo test`. The browser-opening branch was therefore unreachable
    // from a test — the deferred finding from the step 1 review.

    use crate::commands::Deps;
    use crate::confirmation::Opener;
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct RecordingOpener {
        opened: Mutex<Vec<String>>,
    }
    impl Opener for RecordingOpener {
        fn open(&self, url: &str) {
            self.opened.lock().unwrap().push(url.to_string());
        }
    }

    const START_BODY: &str = r#"{"device_code":"dev1","user_code":"ABCD-1234","verification_uri":"https://app.keylight.dev/login/device","verification_uri_complete":"https://app.keylight.dev/login/device?code=ABCD-1234","interval":5,"expires_in":900}"#;
    const APPROVED_BODY: &str =
        r#"{"status":"approved","token":"klm_new","scopes":["usage:read"],"name":"laptop"}"#;

    fn login_ctx(no_browser: bool, is_tty: bool) -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Context {
            json: true, // keep the human progress lines off the test's stdout
            no_browser,
            api_url: "https://api.test".into(),
            config_path: std::env::temp_dir().join(format!("keylight-cli-login-test-{n}.toml")),
            is_tty,
        }
    }

    /// Runs a full approved login and reports which URLs the browser was
    /// asked to open.
    fn run_login(no_browser: bool, is_tty: bool) -> Vec<String> {
        let transport = Arc::new(ScriptedTransport::new(vec![
            resp(200, START_BODY, None),
            resp(200, APPROVED_BODY, None),
        ]));
        let opener = RecordingOpener {
            opened: Mutex::new(Vec::new()),
        };
        let deps = Deps {
            transport,
            sleeper: Arc::new(NoopSleeper::default()),
            opener: &opener,
            max_polls: Some(5),
            notice: Default::default(),
        };
        let ctx = login_ctx(no_browser, is_tty);
        login(&ctx, &deps, Some(vec!["usage:read".into()])).unwrap();
        let _ = crate::config::delete_config(&ctx.config_path);
        let opened = opener.opened.lock().unwrap();
        opened.clone()
    }

    #[test]
    fn at_a_tty_the_browser_opens_on_the_complete_verification_url() {
        // The `_complete` variant is the one that pre-fills the code, so it
        // is what the human should land on when a browser is available.
        assert_eq!(
            run_login(false, true),
            ["https://app.keylight.dev/login/device?code=ABCD-1234"]
        );
    }

    #[test]
    fn no_browser_flag_suppresses_opening_even_at_a_tty() {
        assert!(run_login(true, true).is_empty());
    }

    /// Piped output or a CI log must never pop a browser, flag or no flag.
    #[test]
    fn a_non_tty_suppresses_opening_without_the_flag() {
        assert!(run_login(false, false).is_empty());
    }
}
