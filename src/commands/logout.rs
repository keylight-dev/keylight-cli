//! `keylight logout` — removes the locally stored token.
//!
//! `POST /v1/logout` exists server-side and does revoke the calling token,
//! but this command deliberately does not call it — it only forgets the
//! token locally. Revoking a token server-side is done today from the
//! dashboard's Settings → API tokens page. This is local-only by choice and
//! says so, rather than implying the token is dead everywhere.

use super::Context;
use crate::config;
use crate::error::Result;
use crate::output::emit;

pub fn logout(ctx: &Context) -> Result<()> {
    let had_token = config::read_config(&ctx.config_path)?.token.is_some();
    config::delete_config(&ctx.config_path)?;
    let result = serde_json::json!({
        "removedLocalToken": had_token,
        "serverSideRevocation": "not performed — revoke from the dashboard (Settings > API tokens) if the token may be compromised",
    });
    emit(ctx.json, &result, |_| {
        if had_token {
            println!("Removed the locally stored token.");
        } else {
            println!("No locally stored token to remove.");
        }
        println!(
            "Note: this does not revoke the token server-side. To do that, revoke it \
             from the dashboard (Settings > API tokens)."
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn ctx() -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Context {
            json: false,
            no_browser: true,
            api_url: "https://api.keylight.dev".into(),
            config_path: std::env::temp_dir().join(format!("keylight-cli-logout-test-{n}.toml")),
        }
    }

    #[test]
    fn logout_deletes_the_config_file() {
        let c = ctx();
        config::write_config(
            &c.config_path,
            &ConfigFile {
                token: Some("klm_x".into()),
            },
        )
        .unwrap();
        logout(&c).unwrap();
        assert!(!c.config_path.exists());
    }

    #[test]
    fn logout_is_idempotent_when_nothing_was_stored() {
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        assert!(logout(&c).is_ok());
        assert!(logout(&c).is_ok());
    }
}
