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
/// `scopes: None` omits the field entirely, letting the server apply its own
/// default (every scope except `danger` — `login-device.ts` falls back to
/// `API_TOKEN_SCOPES.filter(s => s !== 'danger')` whenever the field isn't an
/// array). Restating that list here would duplicate a server constant and
/// silently go stale the first time a scope is added.
///
/// `Some(&[])` is a different request: a token with no permissions at all,
/// which is a real thing to ask for and must not be rewritten to the default.
pub fn start(client: &ApiClient, scopes: Option<&[String]>) -> Result<DeviceAuthStart> {
    let body = match scopes {
        Some(s) => serde_json::json!({ "scopes": s }),
        None => serde_json::json!({}),
    };
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
    // Mutable: a `slow_down` response grows this per RFC 8628 §3.5. The
    // attempt budget below is fixed at the *original* interval so a growing
    // interval can't be used to extend the loop indefinitely.
    let mut interval = start.interval.max(1);
    // At least one attempt even if the server sends a nonsense expires_in.
    let max_attempts = (start.expires_in / interval).max(1);
    let body = serde_json::json!({ "device_code": start.device_code });

    for attempt in 0..max_attempts {
        let value = match client.call("POST", "/v1/login/device/token", Some(&body), None) {
            Ok(ApiOutcome::Ok { value, .. }) => value,
            Ok(ApiOutcome::Pending(_)) => return Err(CliError::InvalidResponse),
            // The server signals expiry as a 400 `expired_token` API error
            // (worker/src/routes/v1/login-device.ts:60), not the 200
            // {"status":"expired"} shape handled below. This is the real
            // shape in production.
            Err(CliError::Api {
                status: 400,
                message,
            }) if message == "expired_token" => {
                return Ok(PollOutcome::Expired);
            }
            // The poll endpoint is per-IP rate limited
            // (worker/src/routes/v1/login-device.ts:52-53) and answers
            // `429 slow_down` when tripped — plausible with two developers
            // behind one NAT. The user's code is still valid; back off per
            // RFC 8628 §3.5 and keep polling instead of failing the login.
            Err(CliError::RateLimited { .. }) => {
                interval += 5;
                if attempt + 1 < max_attempts {
                    sleeper.sleep(Duration::from_secs(interval));
                }
                continue;
            }
            Err(e) => return Err(e),
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

        let out = start(&c, Some(&["products:read".to_string()])).unwrap();

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

    /// No `--scope` flag must omit the field entirely rather than restating
    /// the server's default list. `login-device.ts` applies
    /// `API_TOKEN_SCOPES.filter(s => s !== 'danger')` whenever `scopes` isn't
    /// an array, so duplicating that list client-side would silently go stale
    /// the first time a scope is added server-side.
    #[test]
    fn start_omits_scopes_entirely_when_none_were_requested() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, START_BODY, None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );

        start(&c, None).unwrap();

        let reqs = t.requests.lock().unwrap();
        let (_, _, _, body) = &reqs[0];
        assert_eq!(
            body.as_deref().unwrap(),
            "{}",
            "the field must be absent, not an empty array — an empty array \
             asks for a token with no permissions at all"
        );
    }

    #[test]
    fn start_sends_an_empty_scope_array_when_explicitly_asked_for_none() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, START_BODY, None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );

        start(&c, Some(&[])).unwrap();

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

        let err = start(&c, Some(&[])).unwrap_err();
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

    // The poll endpoint rate-limits per IP (worker/src/routes/v1/login-device.ts:52-53,
    // 429 "slow_down" + Retry-After: 5). Two devices behind one NAT can trip
    // this while the user's code is still perfectly valid, so it must back
    // off and keep polling — not die with CliError::RateLimited. Two 429s are
    // scripted for the *first* poll attempt because ApiClient::call itself
    // retries a 429 once before giving up and returning RateLimited.
    #[test]
    fn poll_backs_off_and_continues_on_slow_down_instead_of_dying() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, r#"{"error":"slow_down"}"#, Some(5)),
            resp(429, r#"{"error":"slow_down"}"#, Some(5)),
            resp(
                200,
                r#"{"status":"approved","token":"klm_abc","scopes":[],"name":"Keylight CLI"}"#,
                None,
            ),
        ]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        let noop = Arc::new(NoopSleeper::default());
        let sleeper: Arc<dyn Sleeper> = noop.clone();

        let out = poll_until_complete(&c, &sleeper, &start_fixture()).unwrap();

        assert!(
            matches!(out, PollOutcome::Approved { .. }),
            "slow_down must not end the login while the code is still valid, got {out:?}"
        );
        assert_eq!(
            t.requests.lock().unwrap().len(),
            3,
            "must keep polling after slow_down"
        );

        // `noop` is the sleeper passed to `poll_until_complete` itself, kept
        // separate from the `ApiClient`'s own sleeper (used for its internal
        // 429-retry wait) exactly like the other tests in this module — so
        // this only records poll_until_complete's own backoff sleep, which
        // must be longer than the original 1s interval, proving it grew
        // rather than resetting.
        let slept = noop.calls.lock().unwrap();
        assert_eq!(slept.len(), 1, "expected exactly one backoff sleep");
        assert!(
            slept[0] > Duration::from_secs(start_fixture().interval),
            "interval must grow after slow_down, got {:?}",
            slept[0]
        );
    }

    // The real server never sends the 200 {"status":"expired"} shape today
    // (see worker/src/routes/v1/login-device.ts:60) — it returns a 400
    // `expired_token` API error instead. This must resolve to a friendly
    // `PollOutcome::Expired`, not bubble up as a raw HTTP 400.
    #[test]
    fn poll_honors_an_expired_token_error_from_the_server() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            400,
            r#"{"error":"expired_token"}"#,
            None,
        )]));
        let c = ApiClient::anonymous(t, Arc::new(NoopSleeper::default()), "https://api.test");
        let sleeper: Arc<dyn Sleeper> = Arc::new(NoopSleeper::default());

        assert_eq!(
            poll_until_complete(&c, &sleeper, &start_fixture()).unwrap(),
            PollOutcome::Expired
        );
    }

    // Keep this covered too — the server may add it later, and it's harmless
    // to accept both shapes.
    #[test]
    fn poll_honors_a_200_expired_status_from_the_server() {
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
