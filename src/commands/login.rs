//! `keylight login` — RFC 8628 device authorization.
//!
//! Device flow rather than a loopback callback because the CLI has to work on
//! headless boxes and over SSH, where a browser cannot reach the CLI's
//! localhost. The human step happens on the server's grant page; this module
//! only starts the flow, shows the code, and waits.

use super::Context;
use crate::api::ApiClient;
use crate::config::{self, ConfigFile};
use crate::confirmation::{Opener, RealOpener};
use crate::device_auth::{self, PollOutcome};
use crate::error::{CliError, Result};
use crate::http::ureq_transport::UreqTransport;
use crate::output::emit;
use crate::sleep::{RealSleeper, Sleeper};
use std::io::Write;
use std::sync::Arc;

/// Every scope except `danger`. The grant page pre-checks what is requested and
/// lets the human narrow it, so asking for the working set is the useful
/// default. `danger` is excluded deliberately: the grant page leaves it
/// unchecked, and destructive access should be an explicit request.
pub fn default_scopes() -> Vec<String> {
    [
        "products:read",
        "products:write",
        "licenses:read",
        "licenses:write",
        "customers:read",
        "customers:write",
        "integrations:read",
        "integrations:write",
        "usage:read",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `None` means "no --scope flag given"; `Some(vec![])` means the caller
/// explicitly asked for a token with no permissions, which is a real request.
pub fn resolve_scopes(requested: Option<Vec<String>>) -> Vec<String> {
    requested.unwrap_or_else(default_scopes)
}

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

pub fn login(ctx: &Context, scopes: Option<Vec<String>>) -> Result<()> {
    let transport = Arc::new(UreqTransport::default());
    let sleeper: Arc<dyn Sleeper> = Arc::new(RealSleeper);
    let client = ApiClient::anonymous(transport, sleeper.clone(), ctx.api_url.clone());

    let started = device_auth::start(&client, &resolve_scopes(scopes))?;

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

    if !ctx.no_browser && ctx.is_tty() {
        RealOpener.open(verify_url);
    }

    match device_auth::poll_until_complete(&client, &sleeper, &started)? {
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

    #[test]
    fn default_scopes_cover_the_working_set_and_exclude_danger() {
        let s = default_scopes();
        assert!(s.contains(&"products:read".to_string()));
        assert!(s.contains(&"licenses:write".to_string()));
        assert!(s.contains(&"usage:read".to_string()));
        assert!(
            !s.contains(&"danger".to_string()),
            "danger must stay an explicit opt-in"
        );
    }

    #[test]
    fn explicit_scopes_win_over_the_defaults() {
        assert_eq!(
            resolve_scopes(Some(vec!["usage:read".into()])),
            vec!["usage:read".to_string()]
        );
    }

    #[test]
    fn no_flag_falls_back_to_the_defaults() {
        assert_eq!(resolve_scopes(None), default_scopes());
    }

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

    // An empty --scope list is a meaningful request: a token that can only
    // confirm which account it belongs to. It must not silently become "all".
    #[test]
    fn an_explicitly_empty_scope_list_is_preserved() {
        assert_eq!(resolve_scopes(Some(vec![])), Vec::<String>::new());
    }
}
