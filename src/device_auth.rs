//! RFC 8628 device authorization — the protocol half of `keylight login`.
//!
//! Deliberately knows nothing about `Context`, config files, or printing, so it
//! can be driven entirely by `ScriptedTransport` in tests. The command layer in
//! `commands/login.rs` owns all of that.

use crate::api::{ApiClient, ApiOutcome};
use crate::error::{CliError, Result};
use crate::sleep::Sleeper;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceAuthStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub interval: u64,
    pub expires_in: u64,
}

/// Begin device authorization. Public endpoint — call with
/// [`ApiClient::anonymous`], since there is no token yet.
pub fn start(client: &ApiClient, scopes: &[String]) -> Result<DeviceAuthStart> {
    let body = serde_json::json!({ "scopes": scopes });
    match client.call("POST", "/v1/login/device", Some(&body), None)? {
        ApiOutcome::Ok { value, .. } => {
            serde_json::from_value(value).map_err(|_| CliError::InvalidResponse)
        }
        // A public endpoint has nothing to confirm; treat it as a protocol
        // violation rather than silently continuing.
        ApiOutcome::Pending(_) => Err(CliError::InvalidResponse),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Approved {
        token: String,
        scopes: Vec<String>,
        name: String,
    },
    Denied,
    Expired,
}

#[derive(Deserialize)]
struct PollResponse {
    status: String,
    token: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    name: String,
}

/// Poll until the human approves, denies, or the code expires.
///
/// Bounded by attempt count rather than elapsed wall-clock time: it keeps the
/// loop deterministic under `NoopSleeper`, so the expiry path is testable
/// without sleeping or injecting a clock. A server-side `expired` still wins
/// immediately.
pub fn poll_until_complete(
    client: &ApiClient,
    sleeper: &Arc<dyn Sleeper>,
    start: &DeviceAuthStart,
) -> Result<PollOutcome> {
    let interval = start.interval.max(1);
    // At least one attempt even if the server sends a nonsense expires_in.
    let max_attempts = (start.expires_in / interval).max(1);
    let body = serde_json::json!({ "device_code": start.device_code });

    for attempt in 0..max_attempts {
        let value = match client.call("POST", "/v1/login/device/token", Some(&body), None)? {
            ApiOutcome::Ok { value, .. } => value,
            ApiOutcome::Pending(_) => return Err(CliError::InvalidResponse),
        };
        let parsed: PollResponse =
            serde_json::from_value(value).map_err(|_| CliError::InvalidResponse)?;

        match parsed.status.as_str() {
            "approved" => {
                // The token is returned exactly once; a missing one here means
                // the server said approved but gave us nothing to store.
                let token = parsed.token.ok_or(CliError::InvalidResponse)?;
                return Ok(PollOutcome::Approved {
                    token,
                    scopes: parsed.scopes,
                    name: parsed.name,
                });
            }
            "denied" => return Ok(PollOutcome::Denied),
            "expired" => return Ok(PollOutcome::Expired),
            // "pending", or anything unrecognised — keep waiting rather than
            // failing the login on a status we do not know about yet.
            _ => {}
        }

        // Don't sleep after the final attempt.
        if attempt + 1 < max_attempts {
            sleeper.sleep(Duration::from_secs(interval));
        }
    }

    Ok(PollOutcome::Expired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiClient;
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp};
    use std::sync::Arc;

    const START_BODY: &str = r#"{"device_code":"abc123","user_code":"3DN7-GUUW",
        "verification_uri":"https://app.keylight.dev/login/device",
        "verification_uri_complete":"https://app.keylight.dev/login/device?code=3DN7-GUUW",
        "interval":5,"expires_in":900}"#;

    #[test]
    fn start_posts_scopes_and_parses_the_response() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, START_BODY, None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );

        let out = start(&c, &["products:read".to_string()]).unwrap();

        assert_eq!(out.user_code, "3DN7-GUUW");
        assert_eq!(out.device_code, "abc123");
        assert_eq!(out.interval, 5);
        assert_eq!(out.expires_in, 900);

        let reqs = t.requests.lock().unwrap();
        let (method, url, _, body) = &reqs[0];
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/login/device"), "got {url}");
        assert_eq!(body.as_deref().unwrap(), r#"{"scopes":["products:read"]}"#);
    }

    #[test]
    fn start_sends_an_empty_scope_array_when_none_requested() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, START_BODY, None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );

        start(&c, &[]).unwrap();

        let reqs = t.requests.lock().unwrap();
        let (_, _, _, body) = &reqs[0];
        assert_eq!(body.as_deref().unwrap(), r#"{"scopes":[]}"#);
    }

    #[test]
    fn start_surfaces_an_api_error() {
        // `ApiClient::call` retries a 429 exactly once, so two consecutive
        // 429s must be scripted for this to bottom out in a real error
        // instead of the transport running out of queued responses.
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, r#"{"error":"Too many requests"}"#, None),
            resp(429, r#"{"error":"Too many requests"}"#, None),
        ]));
        let c = ApiClient::anonymous(t, Arc::new(NoopSleeper::default()), "https://api.test");

        let err = start(&c, &[]).unwrap_err();
        assert!(matches!(
            err,
            CliError::RateLimited { .. } | CliError::Api { .. }
        ));
    }

    fn start_fixture() -> DeviceAuthStart {
        DeviceAuthStart {
            device_code: "abc123".into(),
            user_code: "3DN7-GUUW".into(),
            verification_uri: "https://app.keylight.dev/login/device".into(),
            verification_uri_complete: None,
            interval: 1,
            expires_in: 5,
        }
    }

    #[test]
    fn poll_returns_the_token_once_approved() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(200, r#"{"status":"pending"}"#, None),
            resp(200, r#"{"status":"pending"}"#, None),
            resp(
                200,
                r#"{"status":"approved","token":"klm_abc","scopes":["products:read"],"name":"Keylight CLI"}"#,
                None,
            ),
        ]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        let out = poll_until_complete(&c, &sleeper, &start_fixture()).unwrap();

        match out {
            PollOutcome::Approved { token, scopes, .. } => {
                assert_eq!(token, "klm_abc");
                assert_eq!(scopes, vec!["products:read".to_string()]);
            }
            other => panic!("expected Approved, got {other:?}"),
        }
        assert_eq!(t.requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn poll_posts_the_device_code_to_the_token_endpoint() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            200,
            r#"{"status":"approved","token":"klm_abc","scopes":[],"name":"Keylight CLI"}"#,
            None,
        )]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        poll_until_complete(&c, &sleeper, &start_fixture()).unwrap();

        let reqs = t.requests.lock().unwrap();
        let (method, url, _, body) = &reqs[0];
        assert_eq!(method, "POST");
        assert!(url.ends_with("/v1/login/device/token"), "got {url}");
        assert_eq!(body.as_deref().unwrap(), r#"{"device_code":"abc123"}"#);
    }

    #[test]
    fn poll_reports_denial_without_waiting_for_expiry() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            200,
            r#"{"status":"denied"}"#,
            None,
        )]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        assert_eq!(
            poll_until_complete(&c, &sleeper, &start_fixture()).unwrap(),
            PollOutcome::Denied
        );
        assert_eq!(t.requests.lock().unwrap().len(), 1, "must stop on denial");
    }

    // expires_in 5 / interval 1 == 5 attempts, then give up.
    #[test]
    fn poll_gives_up_when_the_code_expires() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(200, r#"{"status":"pending"}"#, None),
            resp(200, r#"{"status":"pending"}"#, None),
            resp(200, r#"{"status":"pending"}"#, None),
            resp(200, r#"{"status":"pending"}"#, None),
            resp(200, r#"{"status":"pending"}"#, None),
        ]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        assert_eq!(
            poll_until_complete(&c, &sleeper, &start_fixture()).unwrap(),
            PollOutcome::Expired
        );
        assert_eq!(t.requests.lock().unwrap().len(), 5);
    }

    // RFC 8628 requires the client to wait the server's `interval` between
    // polls. NoopSleeper records what it was asked to sleep, so this is
    // directly assertable without any real waiting.
    #[test]
    fn poll_waits_the_server_specified_interval_between_attempts() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(200, r#"{"status":"pending"}"#, None),
            resp(
                200,
                r#"{"status":"approved","token":"klm_abc","scopes":[],"name":"Keylight CLI"}"#,
                None,
            ),
        ]));
        let c = ApiClient::anonymous(t, Arc::new(NoopSleeper::default()), "https://api.test");
        let noop = Arc::new(NoopSleeper::default());
        let sleeper: Arc<dyn Sleeper> = noop.clone();

        let mut started = start_fixture();
        started.interval = 7;
        started.expires_in = 70;

        poll_until_complete(&c, &sleeper, &started).unwrap();

        let slept = noop.calls.lock().unwrap();
        assert_eq!(slept.len(), 1, "one wait between the two attempts");
        assert_eq!(slept[0], Duration::from_secs(7));
    }

    // A server-side `expired` beats the local attempt budget.
    #[test]
    fn poll_honors_an_expired_status_from_the_server() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            200,
            r#"{"status":"expired"}"#,
            None,
        )]));
        let c = ApiClient::anonymous(t, Arc::new(NoopSleeper::default()), "https://api.test");
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        assert_eq!(
            poll_until_complete(&c, &sleeper, &start_fixture()).unwrap(),
            PollOutcome::Expired
        );
    }
}
