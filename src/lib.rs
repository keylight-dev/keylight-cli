//! Library surface for the `keylight` binary (`src/main.rs`).
//!
//! Splitting this out (rather than a bin-only crate) matters for two
//! reasons specific to this repo's current stage: the confirm-gated call
//! flow (`confirmation::run_confirmable`) and the generic `api::ApiClient`
//! are **foundation built ahead of its callers** — no resource command
//! (`keylight licenses revoke`, etc.) exists yet to invoke them, since
//! those land once the Phase 2 API surface ships. As `pub` library items
//! they're part of the crate's public interface rather than dead code, and
//! every endpoint command added later is a thin consumer of this module,
//! not a place that reimplements auth/HTTP/confirmation handling.

pub mod api;
pub mod commands;
pub mod config;
pub mod confirmation;
pub mod device_auth;
pub mod endpoints;
pub mod error;
pub mod http;
pub mod output;
pub mod sleep;
pub mod telemetry;
#[cfg(test)]
pub mod test_support;
