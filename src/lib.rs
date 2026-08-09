//! Library surface for the `keylight` binary (`src/main.rs`).
//!
//! Split out from the binary so the internals are testable directly rather
//! than only through the process boundary.
//!
//! Every command is a thin consumer of three things — `api::ApiClient` for
//! the call, `commands::run_gated` for the confirm-gated flow, and
//! `output::emit` for the JSON/human split. No command touches the transport
//! or the config directly, which is what keeps each one independently
//! testable against `test_support::ScriptedTransport`.

pub mod api;
pub mod commands;
pub mod config;
pub mod confirmation;
pub mod device_auth;
pub mod endpoints;
pub mod env_file;
pub mod error;
pub mod http;
pub mod output;
pub mod project_detect;
pub mod sleep;
pub mod telemetry;
#[cfg(test)]
pub mod test_support;
