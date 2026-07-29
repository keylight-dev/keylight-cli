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

pub fn login(ctx: &Context, scopes: Option<Vec<String>>) -> Result<()> {
    let transport = Arc::new(UreqTransport::default());
    let sleeper: Arc<dyn Sleeper> = Arc::new(RealSleeper);
    let client = ApiClient::anonymous(transport, sleeper.clone(), ctx.api_url.clone());

    let started = device_auth::start(&client, &resolve_scopes(scopes))?;

    let verify_url = started
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&started.verification_uri);

    println!("Your code: {}", started.user_code);
    println!("Approve it at: {verify_url}");
    println!("Waiting for approval (Ctrl+C to cancel)...");

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

    // An empty --scope list is a meaningful request: a token that can only
    // confirm which account it belongs to. It must not silently become "all".
    #[test]
    fn an_explicitly_empty_scope_list_is_preserved() {
        assert_eq!(resolve_scopes(Some(vec![])), Vec::<String>::new());
    }
}
