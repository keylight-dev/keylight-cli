//! Token storage and precedence.
//!
//! `KEYLIGHT_API_TOKEN` wins when set (the agent/CI path — no file ever
//! touched). Otherwise the token lives in a small TOML file under the
//! platform config dir (`dirs::config_dir()`, e.g. `~/.config/keylight/config.toml`
//! on Linux, `~/Library/Application Support/keylight/config.toml` on macOS),
//! written with `0600` permissions since it holds a live bearer credential.
//!
//! The resolution and (de)serialization logic here takes its inputs
//! explicitly (an `Option<String>` for the env var, a `&Path` for the file)
//! rather than reading `std::env`/the real filesystem itself, so it's
//! unit-testable without process-global state.

use crate::error::{CliError, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const ENV_TOKEN: &str = "KEYLIGHT_API_TOKEN";

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigFile {
    pub token: Option<String>,
}

/// `~/.config/keylight/config.toml` (platform-appropriate via `dirs`).
/// Returns `None` only if the OS gave us no config dir at all (rare/exotic
/// platforms) — callers surface that as a config error rather than panicking.
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("keylight").join("config.toml"))
}

/// Read and parse the config file. A missing file is `Ok(default)`, not an
/// error — that's the normal state before the first `auth set-token`.
pub fn read_config(path: &Path) -> Result<ConfigFile> {
    match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw).map_err(|e| {
            CliError::Config(format!("invalid config file at {}: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(e) => Err(CliError::Io(e)),
    }
}

/// Write the config file, creating parent directories as needed, restricted
/// to the owner (`0600`) since `token` is a live bearer credential — a
/// leaked `klm_` token grants everything its scopes allow, so this must
/// never be world- or group-readable.
pub fn write_config(path: &Path, cfg: &ConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml = toml::to_string_pretty(cfg)
        .map_err(|e| CliError::Config(format!("failed to serialize config: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(toml.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, toml)?;
    }
    Ok(())
}

/// Delete the config file. Missing is not an error (idempotent logout).
pub fn delete_config(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CliError::Io(e)),
    }
}

/// Token resolution precedence: `KEYLIGHT_API_TOKEN` (non-empty) wins,
/// otherwise fall back to the stored config file's `token`.
pub fn resolve_token(env_token: Option<String>, config_path: &Path) -> Result<Option<String>> {
    if let Some(t) = env_token.filter(|s| !s.is_empty()) {
        return Ok(Some(t));
    }
    Ok(read_config(config_path)?.token)
}

/// `resolve_token`, but a missing token is the specific `CliError::NoToken`
/// rather than `Ok(None)` — the shape every command that needs auth wants.
pub fn require_token(env_token: Option<String>, config_path: &Path) -> Result<String> {
    resolve_token(env_token, config_path)?.ok_or(CliError::NoToken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn tmp_path(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("keylight-cli-test-{name}-{n}.toml"))
    }

    #[test]
    fn env_token_wins_over_config_file() {
        let path = tmp_path("precedence");
        write_config(
            &path,
            &ConfigFile {
                token: Some("klm_from_file".into()),
            },
        )
        .unwrap();
        let resolved = resolve_token(Some("klm_from_env".into()), &path).unwrap();
        assert_eq!(resolved.as_deref(), Some("klm_from_env"));
        let _ = delete_config(&path);
    }

    #[test]
    fn empty_env_token_falls_through_to_file() {
        let path = tmp_path("empty-env");
        write_config(
            &path,
            &ConfigFile {
                token: Some("klm_from_file".into()),
            },
        )
        .unwrap();
        // An empty-string env var (e.g. `KEYLIGHT_API_TOKEN=` in a shell) must
        // not shadow a real stored token.
        let resolved = resolve_token(Some(String::new()), &path).unwrap();
        assert_eq!(resolved.as_deref(), Some("klm_from_file"));
        let _ = delete_config(&path);
    }

    #[test]
    fn falls_back_to_config_file_when_no_env() {
        let path = tmp_path("file-only");
        write_config(
            &path,
            &ConfigFile {
                token: Some("klm_from_file".into()),
            },
        )
        .unwrap();
        let resolved = resolve_token(None, &path).unwrap();
        assert_eq!(resolved.as_deref(), Some("klm_from_file"));
        let _ = delete_config(&path);
    }

    #[test]
    fn missing_config_file_and_no_env_resolves_to_none() {
        let path = tmp_path("missing");
        let _ = delete_config(&path);
        assert_eq!(resolve_token(None, &path).unwrap(), None);
    }

    #[test]
    fn require_token_errors_when_nothing_configured() {
        let path = tmp_path("require-missing");
        let _ = delete_config(&path);
        assert!(matches!(require_token(None, &path), Err(CliError::NoToken)));
    }

    #[test]
    fn require_token_succeeds_when_configured() {
        let path = tmp_path("require-present");
        write_config(
            &path,
            &ConfigFile {
                token: Some("klm_ok".into()),
            },
        )
        .unwrap();
        assert_eq!(require_token(None, &path).unwrap(), "klm_ok");
        let _ = delete_config(&path);
    }

    #[test]
    fn write_then_read_round_trips() {
        let path = tmp_path("round-trip");
        let cfg = ConfigFile {
            token: Some("klm_roundtrip".into()),
        };
        write_config(&path, &cfg).unwrap();
        let read = read_config(&path).unwrap();
        assert_eq!(read, cfg);
        let _ = delete_config(&path);
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_written_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path("perms");
        write_config(
            &path,
            &ConfigFile {
                token: Some("klm_secret".into()),
            },
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = delete_config(&path);
    }

    #[test]
    fn delete_config_is_idempotent_on_missing_file() {
        let path = tmp_path("delete-missing");
        let _ = delete_config(&path);
        assert!(delete_config(&path).is_ok());
    }

    #[test]
    fn read_config_on_missing_file_returns_default() {
        let path = tmp_path("read-missing");
        let _ = delete_config(&path);
        assert_eq!(read_config(&path).unwrap(), ConfigFile::default());
    }
}
