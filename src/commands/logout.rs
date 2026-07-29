//! `keylight logout` — revokes the token server-side, then forgets it locally.
//!
//! Both halves matter, and the local one must happen either way. A logout
//! that leaves the credential working is a security wart: on a shared or
//! borrowed machine, "logged out" has to mean the token stops working, not
//! just that this shell forgot it.
//!
//! But the reverse failure is worse. If the network is down, or the token was
//! already revoked from the dashboard, refusing to clear the local file would
//! leave a dead credential on disk that the user believes they removed. So
//! the revoke is best-effort and the local delete is unconditional — and the
//! output says plainly which of the two actually happened.

use super::{Context, Deps};
use crate::config;
use crate::error::Result;
use crate::output::emit;

pub fn logout(ctx: &Context, deps: &Deps) -> Result<()> {
    let had_token = config::read_config(&ctx.config_path)?.token.is_some()
        || ctx.env_token().is_some_and(|t| !t.is_empty());

    // Best-effort, and only worth attempting if we have something to revoke.
    let revoked = if had_token {
        match ctx.client(deps) {
            Ok(client) => client.call("POST", "/v1/logout", None, None).is_ok(),
            Err(_) => false,
        }
    } else {
        false
    };

    // Unconditional: never leave a credential on disk because a network call
    // failed.
    config::delete_config(&ctx.config_path)?;

    let result = serde_json::json!({
        "removedLocalToken": had_token,
        "revokedServerSide": revoked,
    });
    emit(ctx.json, &result, |_| {
        if !had_token {
            println!("No stored token to remove.");
            return;
        }
        if revoked {
            println!("Logged out. The token is revoked and the local copy is removed.");
        } else {
            println!("Removed the local token, but could not revoke it server-side.");
            println!(
                "If it may be compromised, revoke it from the dashboard \
                 (Settings > API tokens)."
            );
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{RecordingOpener, deps, transport};
    use crate::config::ConfigFile;
    use crate::test_support::resp;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn ctx() -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Context {
            json: false,
            no_browser: true,
            api_url: "https://api.test".into(),
            config_path: std::env::temp_dir().join(format!("keylight-cli-logout-test-{n}.toml")),
            is_tty: false,
        }
    }

    fn with_token(c: &Context) {
        config::write_config(
            &c.config_path,
            &ConfigFile {
                token: Some("klm_x".into()),
            },
        )
        .unwrap();
    }

    /// The endpoint exists precisely so a client can revoke its own token.
    /// Not calling it is what made `keylight logout` leave a live credential
    /// behind — and made the coverage registry's claim about this command false.
    #[test]
    fn logout_revokes_the_token_server_side() {
        let t = transport(vec![resp(200, "{}", None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx();
        with_token(&c);
        logout(&c, &d).unwrap();

        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1, "expected exactly one revoke call");
        assert_eq!(reqs[0].0, "POST");
        assert!(reqs[0].1.ends_with("/v1/logout"), "got {}", reqs[0].1);
        assert!(!c.config_path.exists());
    }

    /// The reverse failure is worse than the first: refusing to clear the
    /// local file when the network is down leaves a dead credential on disk
    /// that the user believes they removed.
    #[test]
    fn the_local_token_is_removed_even_when_the_revoke_call_fails() {
        let t = transport(vec![resp(500, r#"{"error":"boom"}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx();
        with_token(&c);

        logout(&c, &d).unwrap();

        assert!(
            !c.config_path.exists(),
            "a failed revoke must not strand the credential on disk"
        );
    }

    /// An already-revoked token answers 401. That is a successful outcome for
    /// logout, not an error to surface.
    #[test]
    fn an_already_revoked_token_still_logs_out_cleanly() {
        let t = transport(vec![resp(401, r#"{"error":"Unauthorized"}"#, None)]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx();
        with_token(&c);
        assert!(logout(&c, &d).is_ok());
        assert!(!c.config_path.exists());
    }

    /// With nothing stored there is nothing to revoke, so no call is made —
    /// a pointless unauthenticated request would just 401.
    #[test]
    fn logout_with_no_stored_token_makes_no_request() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        // SAFETY: the env token would otherwise make this look like a session.
        unsafe { std::env::remove_var(crate::config::ENV_TOKEN) };

        assert!(logout(&c, &d).is_ok());
        assert!(t.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn logout_is_idempotent() {
        let t = transport(vec![]);
        let o = RecordingOpener::new();
        let d = deps(&t, &o);
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        unsafe { std::env::remove_var(crate::config::ENV_TOKEN) };
        assert!(logout(&c, &d).is_ok());
        assert!(logout(&c, &d).is_ok());
    }
}
