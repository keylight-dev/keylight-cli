//! `keylight auth set-token` / `keylight auth status`.

use super::Context;
use crate::api::{ApiClient, ApiOutcome};
use crate::config::{self, ConfigFile};
use crate::error::{CliError, Result};
use crate::http::ureq_transport::UreqTransport;
use crate::output::emit;
use crate::sleep::RealSleeper;
use std::io::BufRead;
use std::sync::Arc;

/// Read the token from stdin — **never** from an argv/flag value, since
/// process arguments land in shell history and are visible to other users
/// via `ps`. Works both interactively (`keylight auth set-token` then paste
/// and Enter) and piped (`echo "$TOKEN" | keylight auth set-token`, the
/// CI path).
pub fn set_token(ctx: &Context, reader: &mut dyn BufRead) -> Result<()> {
    if ctx.is_tty() {
        eprintln!("Paste your Keylight API token (klm_...), then press Enter:");
    }
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let token = line.trim().to_string();
    if token.is_empty() {
        return Err(CliError::Config("no token provided on stdin".into()));
    }
    if !token.starts_with("klm_") {
        eprintln!(
            "warning: token doesn't start with the expected `klm_` prefix — storing it anyway"
        );
    }
    config::write_config(&ctx.config_path, &ConfigFile { token: Some(token) })?;
    let result =
        serde_json::json!({ "stored": true, "path": ctx.config_path.display().to_string() });
    emit(ctx.json, &result, |_| {
        println!(
            "Token stored at {} (permissions 0600).",
            ctx.config_path.display()
        );
    });
    Ok(())
}

/// `GET /v1/tokens/verify` — who am I: tenant, token id, scopes.
pub fn status(ctx: &Context) -> Result<()> {
    let token = config::require_token(ctx.env_token(), &ctx.config_path)?;
    let client = ApiClient::new(
        Arc::new(UreqTransport::default()),
        Arc::new(RealSleeper),
        ctx.api_url.clone(),
        token,
    );
    let value = match client.get("/v1/tokens/verify")? {
        ApiOutcome::Ok { value, .. } => value,
        // /v1/tokens/verify is a plain (non-gated) endpoint per the API
        // design; a 202 here would mean the server misconfigured it.
        ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
    };
    emit(ctx.json, &value, |v| {
        let tenant = v.get("tenantId").and_then(|x| x.as_str()).unwrap_or("?");
        let token_id = v.get("tokenId").and_then(|x| x.as_str()).unwrap_or("?");
        let scopes = v
            .get("scopes")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("Tenant:  {tenant}");
        println!("Token:   {token_id}");
        println!(
            "Scopes:  {}",
            if scopes.is_empty() { "(none)" } else { &scopes }
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn ctx() -> Context {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Context {
            json: false,
            no_browser: true,
            api_url: "https://api.keylight.dev".into(),
            config_path: std::env::temp_dir().join(format!("keylight-cli-auth-test-{n}.toml")),
        }
    }

    #[test]
    fn set_token_reads_from_stdin_not_args() {
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        let mut input = Cursor::new(b"klm_abc123\n".to_vec());
        set_token(&c, &mut input).unwrap();
        let stored = config::read_config(&c.config_path).unwrap();
        assert_eq!(stored.token.as_deref(), Some("klm_abc123"));
        let _ = config::delete_config(&c.config_path);
    }

    #[test]
    fn set_token_trims_trailing_whitespace_and_newline() {
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        let mut input = Cursor::new(b"  klm_padded  \n".to_vec());
        set_token(&c, &mut input).unwrap();
        let stored = config::read_config(&c.config_path).unwrap();
        assert_eq!(stored.token.as_deref(), Some("klm_padded"));
        let _ = config::delete_config(&c.config_path);
    }

    #[test]
    fn set_token_rejects_empty_stdin() {
        let c = ctx();
        let _ = config::delete_config(&c.config_path);
        let mut input = Cursor::new(b"\n".to_vec());
        assert!(set_token(&c, &mut input).is_err());
    }
}
