//! The CLI error type [`CliError`] and its [`Result`] alias.
//!
//! Mirrors the shape of `keylight::KeylightError` in the Rust SDK (same
//! house style: `thiserror`, one variant per failure class the caller might
//! want to branch on) rather than a single opaque `anyhow::Error`, since
//! `main` needs to map specific variants to specific exit codes and messages.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    /// A non-2xx, non-202 JSON response from the API. `message` is the
    /// `{"error": "..."}` envelope's message, or a fallback when the body
    /// didn't parse as that envelope.
    #[error("HTTP {status}: {message}")]
    Api { status: u16, message: String },

    #[error("rate limited; retry after {retry_after}s")]
    RateLimited { retry_after: u64 },

    #[error("network error: {0}")]
    Network(String),

    #[error("invalid response from server")]
    InvalidResponse,

    #[error("no API token configured; run `keylight auth set-token` or set KEYLIGHT_API_TOKEN")]
    NoToken,

    #[error("confirmation {status}: {description}")]
    ConfirmationNotApproved { status: String, description: String },

    #[error("confirmation timed out waiting for approval")]
    ConfirmationTimedOut,

    /// The command was understood, but its inputs do not make a safe setup
    /// check possible (for example, several apps without `--product`).
    #[error("{0}")]
    UsageError(String),

    /// The command completed its setup calls but the SDK could not prove the
    /// returned lease is valid and trusted.
    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("{0}")]
    Config(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CliError>;

impl CliError {
    /// Stable exit codes for scripts and agents.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoToken
            | Self::Api { status: 401, .. }
            | Self::Api { status: 403, .. }
            | Self::UsageError(_) => 2,
            Self::ConfirmationNotApproved { .. } | Self::ConfirmationTimedOut => 3,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CliError;

    #[test]
    fn setup_error_exit_codes_are_stable() {
        assert_eq!(CliError::UsageError("choose an app".into()).exit_code(), 2);
        assert_eq!(
            CliError::VerificationFailed("bad lease".into()).exit_code(),
            1
        );
        assert_eq!(CliError::ConfirmationTimedOut.exit_code(), 3);
    }
}
