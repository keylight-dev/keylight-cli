pub mod auth;
pub mod login;
pub mod logout;

use crate::api::ApiClient;
use crate::config;
use crate::confirmation::{
    ConfirmOptions, Opener, RealOpener, run_confirmable, run_confirmable_raw,
};
use crate::error::Result;
use crate::http::Transport;
use crate::http::ureq_transport::UreqTransport;
use crate::sleep::{RealSleeper, Sleeper};
use std::path::PathBuf;
use std::sync::Arc;

/// Everything a command needs, resolved once in `main` from CLI flags + env.
pub struct Context {
    pub json: bool,
    pub no_browser: bool,
    pub api_url: String,
    pub config_path: PathBuf,
    /// True when stdout is an interactive terminal — gates browser-opening
    /// (never pop a browser in a CI log or a piped script) alongside the
    /// explicit `--no-browser` flag.
    ///
    /// A resolved field rather than a method reading the real stdout, so a
    /// test can exercise both sides of the gate. As a method it was
    /// unconditionally `false` under `cargo test`, which meant the
    /// browser-opening branch had no coverage at all.
    pub is_tty: bool,
}

impl Context {
    pub fn env_token(&self) -> Option<String> {
        std::env::var(config::ENV_TOKEN).ok()
    }

    /// An authenticated client, or `CliError::NoToken`. Every command needs
    /// exactly this, and repeating the construction inline is what made the
    /// TTY/browser gate untestable in `login` — the transport was baked in at
    /// the call site with no seam to substitute.
    pub fn client(&self, deps: &Deps) -> Result<ApiClient> {
        let token = config::require_token(self.env_token(), &self.config_path)?;
        Ok(ApiClient::new(
            deps.transport.clone(),
            deps.sleeper.clone(),
            self.api_url.clone(),
            token,
        ))
    }

    /// A client for the two endpoints that take no auth — the device-login
    /// start/poll pair.
    pub fn anonymous_client(&self, deps: &Deps) -> ApiClient {
        ApiClient::anonymous(
            deps.transport.clone(),
            deps.sleeper.clone(),
            self.api_url.clone(),
        )
    }
}

/// The real browser opener. A unit struct, so it can live in a `static` and
/// be borrowed by [`Deps::real`] without an allocation or a lifetime knot.
pub static REAL_OPENER: RealOpener = RealOpener;

/// The side-effecting collaborators a command needs: HTTP, sleeping, and
/// opening a browser. Injected rather than constructed in place so that the
/// paths which are otherwise untestable — the TTY/no-browser gate, the
/// confirm-gated poll loop — can be driven from a test.
pub struct Deps<'a> {
    pub transport: Arc<dyn Transport>,
    pub sleeper: Arc<dyn Sleeper>,
    pub opener: &'a dyn Opener,
    /// Safety valve for the confirmation poll loop; `None` in production,
    /// where the server-side confirmation expires on its own.
    pub max_polls: Option<u32>,
}

impl Deps<'static> {
    /// The production wiring: real HTTP, real sleeping, real browser.
    pub fn real() -> Self {
        Deps {
            transport: Arc::new(UreqTransport::default()),
            sleeper: Arc::new(RealSleeper),
            opener: &REAL_OPENER,
            max_polls: None,
        }
    }
}

/// Render the "this needs approval" notice.
///
/// Always stderr, in both output modes. Under `--json` stdout must carry only
/// the result object; and `keylight licenses export > licenses.csv` must put
/// CSV in the file, not an approval URL. Progress is not a result.
fn on_pending_notice(pending: &crate::api::PendingConfirmation) {
    let minutes = pending.expires_in_seconds / 60;
    eprintln!("This operation needs approval in your browser.");
    eprintln!("Approve it at: {}", pending.approve_url);
    eprintln!("Waiting for approval (expires in {minutes} min, Ctrl+C to cancel)...");
}

fn confirm_opts<'a>(ctx: &Context, deps: &'a Deps<'a>) -> ConfirmOptions<'a> {
    ConfirmOptions {
        no_browser: ctx.no_browser,
        is_tty: ctx.is_tty,
        opener: deps.opener,
        sleeper: deps.sleeper.as_ref(),
        on_pending: &on_pending_notice,
        max_polls: deps.max_polls,
    }
}

/// Run a call that may be confirm-gated, walking the approval flow if the
/// server answers `202`. Ungated endpoints go through unchanged, so a command
/// never has to know which tier it is calling.
pub fn run_gated(
    ctx: &Context,
    deps: &Deps,
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    run_confirmable(client, method, path, body, &confirm_opts(ctx, deps))
}

/// As [`run_gated`], but returns the success body verbatim. Only
/// `licenses export` needs it — the response is `text/csv`.
pub fn run_gated_raw(
    ctx: &Context,
    deps: &Deps,
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<String> {
    run_confirmable_raw(client, method, path, body, &confirm_opts(ctx, deps))
}

/// Build a JSON object from only the fields that were actually supplied.
///
/// `PATCH` endpoints treat a present key as "set it" — sending `null` for an
/// unsupplied flag would clear a field the user never mentioned. Every
/// optional flag goes through here.
pub fn json_body(fields: Vec<(&str, Option<serde_json::Value>)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in fields {
        if let Some(v) = v {
            map.insert(k.to_string(), v);
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_body_omits_unsupplied_fields_entirely() {
        let body = json_body(vec![
            ("supportEmail", Some(serde_json::json!("a@b.com"))),
            ("purchaseURL", None),
        ]);
        assert_eq!(body, serde_json::json!({"supportEmail": "a@b.com"}));
        assert!(
            !body.as_object().unwrap().contains_key("purchaseURL"),
            "an unsupplied field must be absent, not null — PATCH treats a \
             present null as an instruction to clear the field"
        );
    }

    /// An explicit null is different from an unsupplied field: `webhook set
    /// --url ""` clears the webhook, and that must survive to the wire.
    #[test]
    fn json_body_keeps_an_explicit_null() {
        let body = json_body(vec![("url", Some(serde_json::Value::Null))]);
        assert_eq!(body, serde_json::json!({ "url": null }));
    }

    #[test]
    fn json_body_with_nothing_supplied_is_an_empty_object() {
        let body = json_body(vec![("a", None), ("b", None)]);
        assert_eq!(body, serde_json::json!({}));
    }
}
