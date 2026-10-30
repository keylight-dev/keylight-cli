//! Transport abstraction. Same shape as `keylight::http::Transport` in the
//! Rust SDK — a minimal blocking trait the rest of the CLI codes against, so
//! tests can substitute a mock instead of hitting the network. Unlike the
//! SDK (which only ever POSTs or GETs against a fixed license-verb surface),
//! the management API needs the full verb set (GET/POST/PATCH/PUT/DELETE),
//! so `request` takes the method explicitly.

pub mod ureq_transport;

/// Minimal HTTP response the CLI reasons about.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<u64>,
    /// Value of the `Keylight-Notice` response header, when the server sent one.
    pub notice: Option<String>,
}

/// Outcome of a transport attempt: an HTTP response, or a transient/terminal
/// network failure below the HTTP layer.
pub enum TransportOutcome {
    Response(HttpResponse),
    /// Retryable transport-level failure (timeout, connection lost, DNS, ...).
    Transient(String),
    /// Non-retryable transport failure.
    Terminal(String),
}

/// A blocking HTTP transport. Implemented by `ureq_transport::UreqTransport`;
/// mocked in tests (see `tests/` and the `#[cfg(test)]` modules under `src/`).
pub trait Transport: Send + Sync {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
    ) -> TransportOutcome;
}
