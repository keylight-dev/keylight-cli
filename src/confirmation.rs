//! The confirm-gated call flow (npm `web-auth`-style), reusable by any
//! command that hits a tier-2 endpoint: `revoke license`, `delete key type`,
//! `rotate provider secret`, `set webhook URL`, `sdk-key read/rotate`,
//! `licenses export`.
//!
//! 1. Call the endpoint with no `X-Keylight-Confirmation` header.
//! 2. A `202` means it's gated: print the human-readable approval URL (and
//!    open a browser unless `--no-browser` or stdout isn't a TTY).
//! 3. Poll `GET /v1/confirmations/:id`, sleeping the server's `Retry-After`
//!    each round, until `approved` / `denied` / `expired`.
//! 4. On `approved`, retry the *original* call with
//!    `X-Keylight-Confirmation: <id>` and return that result.
//!
//! Browser-opening and printing are injected (`Opener`, `on_pending`) rather
//! than called directly, so the whole state machine — including the "denied"
//! and "expired" terminal branches the design spec calls out — is
//! unit-testable against a scripted mock transport, no real browser or
//! sleep involved.

use crate::api::{ApiClient, ApiOutcome, PendingConfirmation, RawOutcome};
use crate::error::{CliError, Result};
use crate::sleep::Sleeper;
use std::time::Duration;

/// Default poll interval when the server didn't send `Retry-After` — should
/// not normally happen (the route always sets it), but a sane floor keeps a
/// buggy/older server from causing a busy-loop.
const DEFAULT_POLL_SECONDS: u64 = 5;

pub trait Opener: Send + Sync {
    fn open(&self, url: &str);
}

pub struct RealOpener;
impl Opener for RealOpener {
    fn open(&self, url: &str) {
        // Best-effort: a failed browser launch is not fatal, the URL is
        // already printed for the human to open by hand.
        let _ = open::that(url);
    }
}

pub struct ConfirmOptions<'a> {
    pub no_browser: bool,
    pub is_tty: bool,
    pub opener: &'a dyn Opener,
    pub sleeper: &'a dyn Sleeper,
    /// Called once, right after the server returns 202, with the pending
    /// confirmation — the caller decides how to render it (human text vs
    /// `--json`).
    pub on_pending: &'a dyn Fn(&PendingConfirmation),
    /// Safety valve against an infinite loop on a misbehaving server; `None`
    /// polls until `approved`/`denied`/`expired` (production default — the
    /// server-side confirmation itself expires after 10 minutes, so this
    /// terminates one way or another).
    pub max_polls: Option<u32>,
}

/// One leg of a gated call: either the endpoint answered, or it wants a
/// confirmation first. Lets the JSON and raw entry points share one copy of
/// the state machine instead of forking it — the CSV export is gated by the
/// same middleware as every other tier-2 operation.
enum Step<T> {
    Done(T),
    Gated(PendingConfirmation),
}

/// Run `method path body`, transparently walking the confirm-gated flow if
/// the server responds `202`. Returns the eventual success body.
pub fn run_confirmable(
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    opts: &ConfirmOptions,
) -> Result<serde_json::Value> {
    drive(client, opts, |confirmation_id| {
        Ok(match client.call(method, path, body, confirmation_id)? {
            ApiOutcome::Ok { value, .. } => Step::Done(value),
            ApiOutcome::Pending(p) => Step::Gated(p),
        })
    })
}

/// As [`run_confirmable`], but hands back the success body verbatim.
/// `GET /v1/licenses/export` answers `text/csv`, which a JSON parse would
/// reject as an invalid response.
pub fn run_confirmable_raw(
    client: &ApiClient,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    opts: &ConfirmOptions,
) -> Result<String> {
    drive(client, opts, |confirmation_id| {
        Ok(
            match client.call_raw(method, path, body, confirmation_id)? {
                RawOutcome::Ok(text) => Step::Done(text),
                RawOutcome::Pending(p) => Step::Gated(p),
            },
        )
    })
}

fn drive<T>(
    client: &ApiClient,
    opts: &ConfirmOptions,
    mut call: impl FnMut(Option<&str>) -> Result<Step<T>>,
) -> Result<T> {
    match call(None)? {
        Step::Done(v) => Ok(v),
        Step::Gated(pending) => {
            (opts.on_pending)(&pending);
            if !opts.no_browser && opts.is_tty {
                opts.opener.open(&pending.approve_url);
            }
            wait_for_approval(client, &pending, opts)?;
            match call(Some(&pending.id))? {
                Step::Done(v) => Ok(v),
                // A server that re-pends a call carrying a just-consumed
                // confirmation id is misbehaving relative to the design spec
                // (consumption is atomic and single-use); surface that as an
                // invalid-response condition rather than looping forever.
                Step::Gated(_) => Err(CliError::InvalidResponse),
            }
        }
    }
}

fn wait_for_approval(
    client: &ApiClient,
    pending: &PendingConfirmation,
    opts: &ConfirmOptions,
) -> Result<()> {
    let poll_path = format!("/v1/confirmations/{}", pending.id);
    let mut polls = 0u32;
    loop {
        if let Some(max) = opts.max_polls {
            if polls >= max {
                return Err(CliError::ConfirmationTimedOut);
            }
        }
        polls += 1;
        let (status, retry_after) = match client.call("GET", &poll_path, None, None)? {
            ApiOutcome::Ok { value, retry_after } => {
                let status = value
                    .get("status")
                    .and_then(|s| s.as_str())
                    .ok_or(CliError::InvalidResponse)?
                    .to_string();
                (status, retry_after)
            }
            ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
        };
        match status.as_str() {
            "approved" => return Ok(()),
            "denied" | "expired" => {
                return Err(CliError::ConfirmationNotApproved {
                    status,
                    description: pending.id.clone(),
                });
            }
            _ => {
                let wait = retry_after.unwrap_or(DEFAULT_POLL_SECONDS);
                opts.sleeper.sleep(Duration::from_secs(wait));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::{Arc, Mutex};

    struct RecordingOpener {
        opened: Mutex<Vec<String>>,
    }
    impl RecordingOpener {
        fn new() -> Self {
            Self {
                opened: Mutex::new(Vec::new()),
            }
        }
    }
    impl Opener for RecordingOpener {
        fn open(&self, url: &str) {
            self.opened.lock().unwrap().push(url.to_string());
        }
    }

    const PENDING_BODY: &str = r#"{"confirmation":{"id":"conf1","status":"pending","approveUrl":"https://app.keylight.dev/dashboard/t1/confirmations/conf1","expiresInSeconds":600}}"#;

    #[test]
    fn immediate_2xx_skips_the_confirmation_flow_entirely() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            200,
            r#"{"ok":true}"#,
            None,
        )]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: false,
            is_tty: true,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| panic!("on_pending must not fire for an immediate 2xx"),
            max_polls: Some(5),
        };
        let out = run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap();
        assert_eq!(out["ok"], true);
        assert!(opener.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn pending_then_approved_retries_with_confirmation_header_and_returns_result() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),                  // initial call: gated
            resp(200, r#"{"status":"pending"}"#, Some(5)),  // poll 1
            resp(200, r#"{"status":"approved"}"#, Some(5)), // poll 2
            resp(200, r#"{"revoked":true}"#, None),         // retried call, approved
        ]));
        let client = ApiClient::new(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: false,
            is_tty: true,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(10),
        };
        let out = run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap();
        assert_eq!(out["revoked"], true);

        // Browser opened exactly once, with the server's approveUrl.
        assert_eq!(
            opener.opened.lock().unwrap().as_slice(),
            ["https://app.keylight.dev/dashboard/t1/confirmations/conf1"]
        );
        // Polling slept using the server's Retry-After (5s) each of the two
        // non-terminal polls.
        assert_eq!(
            sleeper.calls.lock().unwrap().as_slice(),
            [Duration::from_secs(5)]
        );
        // Final retried call carried the confirmation header.
        let reqs = t.requests.lock().unwrap();
        let (_, _, last_headers, _) = reqs.last().unwrap();
        assert!(last_headers.contains(&(
            crate::api::CONFIRMATION_HEADER.to_string(),
            "conf1".to_string()
        )));
    }

    #[test]
    fn denied_confirmation_is_a_terminal_error_not_a_retry() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),
            resp(200, r#"{"status":"denied"}"#, Some(5)),
        ]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: false,
            is_tty: true,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(10),
        };
        let err =
            run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap_err();
        assert!(matches!(
            err,
            CliError::ConfirmationNotApproved { status, .. } if status == "denied"
        ));
    }

    #[test]
    fn expired_confirmation_is_a_terminal_error() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),
            resp(200, r#"{"status":"expired"}"#, Some(5)),
        ]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: false,
            is_tty: true,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(10),
        };
        let err =
            run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap_err();
        assert!(matches!(
            err,
            CliError::ConfirmationNotApproved { status, .. } if status == "expired"
        ));
    }

    #[test]
    fn no_browser_flag_suppresses_opening() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),
            resp(200, r#"{"status":"approved"}"#, Some(5)),
            resp(200, r#"{"revoked":true}"#, None),
        ]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: true,
            is_tty: true,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(10),
        };
        run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap();
        assert!(opener.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn non_tty_suppresses_opening_even_without_no_browser_flag() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),
            resp(200, r#"{"status":"approved"}"#, Some(5)),
            resp(200, r#"{"revoked":true}"#, None),
        ]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: false,
            is_tty: false,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(10),
        };
        run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap();
        assert!(opener.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn max_polls_bounds_a_misbehaving_server_instead_of_looping_forever() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(202, PENDING_BODY, None),
            resp(200, r#"{"status":"pending"}"#, Some(1)),
            resp(200, r#"{"status":"pending"}"#, Some(1)),
        ]));
        let client = ApiClient::new(
            t,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        );
        let sleeper = NoopSleeper::default();
        let opener = RecordingOpener::new();
        let opts = ConfirmOptions {
            no_browser: true,
            is_tty: false,
            opener: &opener,
            sleeper: &sleeper,
            on_pending: &|_| {},
            max_polls: Some(2),
        };
        let err =
            run_confirmable(&client, "POST", "/v1/licenses/L1/revoke", None, &opts).unwrap_err();
        assert!(matches!(err, CliError::ConfirmationTimedOut));
    }
}
