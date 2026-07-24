//! Thin JSON client over the management API: bearer auth, the `{"error":
//! "..."}` envelope, and the `202` confirmation envelope. Retry policy is
//! deliberately simple compared to the SDK's exponential backoff — the
//! management API's rate limiter is a per-minute token bucket
//! (`checkAndIncrementRateLimit`), so a single wait-for-`Retry-After`-then-retry
//! is the documented behavior (design spec: "429 Retry-After honoring (single
//! retry)"), not a multi-attempt loop.

use crate::error::{CliError, Result};
use crate::http::{Transport, TransportOutcome};
use crate::sleep::Sleeper;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Header the server expects on the retried call once a confirmation has
/// been approved. Matches `worker/src/middleware/require-confirmation.ts`.
pub const CONFIRMATION_HEADER: &str = "X-Keylight-Confirmation";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PendingConfirmation {
    pub id: String,
    pub status: String,
    #[serde(rename = "approveUrl")]
    pub approve_url: String,
    #[serde(rename = "expiresInSeconds")]
    pub expires_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct ConfirmationEnvelope {
    confirmation: PendingConfirmation,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: Option<String>,
}

/// Result of a single API call. `Pending` is only ever returned for a
/// confirm-gated endpoint's initial (no `X-Keylight-Confirmation` header)
/// request; retried calls that carry the header resolve to `Ok` or an error.
#[derive(Debug)]
pub enum ApiOutcome {
    Ok {
        value: serde_json::Value,
        retry_after: Option<u64>,
    },
    Pending(PendingConfirmation),
}

pub struct ApiClient {
    transport: Arc<dyn Transport>,
    sleeper: Arc<dyn Sleeper>,
    base_url: String,
    token: String,
}

impl ApiClient {
    pub fn new(
        transport: Arc<dyn Transport>,
        sleeper: Arc<dyn Sleeper>,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            sleeper,
            base_url: base_url.into(),
            token: token.into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn headers(&self, confirmation_id: Option<&str>) -> Vec<(String, String)> {
        let mut h = vec![
            ("Authorization".into(), format!("Bearer {}", self.token)),
            ("Content-Type".into(), "application/json".into()),
        ];
        if let Some(id) = confirmation_id {
            h.push((CONFIRMATION_HEADER.into(), id.into()));
        }
        h
    }

    /// `method` is an HTTP verb (`"GET"`, `"POST"`, `"PATCH"`, `"PUT"`,
    /// `"DELETE"`); `confirmation_id`, when set, is sent as
    /// `X-Keylight-Confirmation` — the retried leg of a confirm-gated call.
    pub fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        confirmation_id: Option<&str>,
    ) -> Result<ApiOutcome> {
        let url = self.url(path);
        let headers = self.headers(confirmation_id);
        let body_str = body.map(|b| b.to_string());
        let outcome = self
            .transport
            .request(method, &url, &headers, body_str.as_deref());
        self.handle_outcome(outcome, method, &url, &headers, body_str.as_deref(), 1)
    }

    fn handle_outcome(
        &self,
        outcome: TransportOutcome,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
        attempt: u32,
    ) -> Result<ApiOutcome> {
        match outcome {
            TransportOutcome::Response(r) if r.status == 202 => {
                let env: ConfirmationEnvelope =
                    serde_json::from_str(&r.body).map_err(|_| CliError::InvalidResponse)?;
                Ok(ApiOutcome::Pending(env.confirmation))
            }
            TransportOutcome::Response(r) if (200..300).contains(&r.status) => {
                let value = if r.body.trim().is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_str(&r.body).map_err(|_| CliError::InvalidResponse)?
                };
                Ok(ApiOutcome::Ok {
                    value,
                    retry_after: r.retry_after,
                })
            }
            TransportOutcome::Response(r) if r.status == 429 && attempt == 1 => {
                // Single retry, honoring the server's Retry-After (defaulting to
                // 1s if it sent none — never busy-loop on a 429).
                let wait = r.retry_after.unwrap_or(1);
                self.sleeper.sleep(Duration::from_secs(wait));
                let retry_outcome = self.transport.request(method, url, headers, body);
                self.handle_outcome(retry_outcome, method, url, headers, body, attempt + 1)
            }
            TransportOutcome::Response(r) if r.status == 429 => Err(CliError::RateLimited {
                retry_after: r.retry_after.unwrap_or(0),
            }),
            TransportOutcome::Response(r) => {
                let message = serde_json::from_str::<ErrorEnvelope>(&r.body)
                    .ok()
                    .and_then(|e| e.error)
                    .unwrap_or_else(|| {
                        format!("request failed with no error message (HTTP {})", r.status)
                    });
                Err(CliError::Api {
                    status: r.status,
                    message,
                })
            }
            TransportOutcome::Transient(e) | TransportOutcome::Terminal(e) => {
                Err(CliError::Network(e))
            }
        }
    }

    pub fn get(&self, path: &str) -> Result<ApiOutcome> {
        self.call("GET", path, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use crate::sleep::NoopSleeper;
    use std::sync::Mutex;

    /// A recorded request: method, url, headers, body.
    type RecordedRequest = (String, String, Vec<(String, String)>, Option<String>);

    /// Scripted transport: returns queued responses in order, and records
    /// every request it received (method/url/headers/body) for assertions —
    /// same shape as the SDK's `CapturingOk`/`FlakyTransport` test doubles.
    struct ScriptedTransport {
        responses: Mutex<Vec<TransportOutcome>>,
        requests: Mutex<Vec<RecordedRequest>>,
    }
    impl ScriptedTransport {
        fn new(responses: Vec<TransportOutcome>) -> Self {
            Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }
    }
    impl Transport for ScriptedTransport {
        fn request(
            &self,
            method: &str,
            url: &str,
            headers: &[(String, String)],
            body: Option<&str>,
        ) -> TransportOutcome {
            self.requests.lock().unwrap().push((
                method.to_string(),
                url.to_string(),
                headers.to_vec(),
                body.map(|s| s.to_string()),
            ));
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                panic!("ScriptedTransport ran out of queued responses");
            }
            responses.remove(0)
        }
    }

    fn resp(status: u16, body: &str, retry_after: Option<u64>) -> TransportOutcome {
        TransportOutcome::Response(HttpResponse {
            status,
            body: body.into(),
            retry_after,
        })
    }

    fn client(transport: Arc<ScriptedTransport>) -> ApiClient {
        ApiClient::new(
            transport,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        )
    }

    #[test]
    fn ok_response_parses_json_body() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            200,
            r#"{"tenantId":"t1","scopes":["products:read"]}"#,
            None,
        )]));
        let c = client(t);
        let outcome = c.get("/v1/tokens/verify").unwrap();
        match outcome {
            ApiOutcome::Ok { value, .. } => {
                assert_eq!(value["tenantId"], "t1");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn bearer_header_is_sent() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let c = client(t.clone());
        c.get("/v1/tokens/verify").unwrap();
        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        assert!(headers.contains(&("Authorization".to_string(), "Bearer klm_test".to_string())));
    }

    #[test]
    fn error_envelope_message_is_extracted() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            403,
            r#"{"error":"Missing scope: licenses:write"}"#,
            None,
        )]));
        let c = client(t);
        let err = c.get("/v1/licenses").unwrap_err();
        match err {
            CliError::Api { status, message } => {
                assert_eq!(status, 403);
                assert_eq!(message, "Missing scope: licenses:write");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn error_envelope_missing_message_falls_back() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(500, "not json", None)]));
        let c = client(t);
        let err = c.get("/v1/licenses").unwrap_err();
        assert!(matches!(err, CliError::Api { status: 500, .. }));
    }

    #[test]
    fn not_found_maps_to_api_error() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(
            404,
            r#"{"error":"Not found"}"#,
            None,
        )]));
        let c = client(t);
        let err = c.get("/v1/products/missing").unwrap_err();
        assert!(matches!(err, CliError::Api { status: 404, .. }));
    }

    #[test]
    fn rate_limit_is_retried_exactly_once_and_then_succeeds() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, r#"{"error":"Rate limited"}"#, Some(2)),
            resp(200, r#"{"ok":true}"#, None),
        ]));
        let c = client(t.clone());
        let outcome = c.get("/v1/licenses").unwrap();
        assert!(matches!(outcome, ApiOutcome::Ok { .. }));
        assert_eq!(
            t.requests.lock().unwrap().len(),
            2,
            "expected exactly one retry"
        );
    }

    #[test]
    fn rate_limit_retry_honors_retry_after_seconds() {
        let sleeper = Arc::new(NoopSleeper::default());
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, "{}", Some(7)),
            resp(200, "{}", None),
        ]));
        let c = ApiClient::new(t, sleeper.clone(), "https://api.keylight.dev", "klm_test");
        c.get("/v1/licenses").unwrap();
        let calls = sleeper.calls.lock().unwrap();
        assert_eq!(calls.as_slice(), [Duration::from_secs(7)]);
    }

    #[test]
    fn second_consecutive_429_is_not_retried_again() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, "{}", Some(1)),
            resp(429, "{}", Some(1)),
        ]));
        let c = client(t.clone());
        let err = c.get("/v1/licenses").unwrap_err();
        assert!(matches!(err, CliError::RateLimited { retry_after: 1 }));
        assert_eq!(
            t.requests.lock().unwrap().len(),
            2,
            "must not retry a second time"
        );
    }

    #[test]
    fn confirmation_202_parses_into_pending() {
        let body = r#"{"confirmation":{"id":"abc123","status":"pending","approveUrl":"https://app.keylight.dev/dashboard/t1/confirmations/abc123","expiresInSeconds":600}}"#;
        let t = Arc::new(ScriptedTransport::new(vec![resp(202, body, Some(5))]));
        let c = client(t);
        let outcome = c
            .call("POST", "/v1/licenses/L1/revoke", None, None)
            .unwrap();
        match outcome {
            ApiOutcome::Pending(p) => {
                assert_eq!(p.id, "abc123");
                assert_eq!(p.status, "pending");
                assert_eq!(p.expires_in_seconds, 600);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[test]
    fn confirmation_header_is_sent_on_retry_leg() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let c = client(t.clone());
        c.call("POST", "/v1/licenses/L1/revoke", None, Some("abc123"))
            .unwrap();
        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        assert!(headers.contains(&(CONFIRMATION_HEADER.to_string(), "abc123".to_string())));
    }

    #[test]
    fn network_failure_maps_to_network_error() {
        let t = Arc::new(ScriptedTransport::new(vec![TransportOutcome::Transient(
            "connection reset".into(),
        )]));
        let c = client(t);
        let err = c.get("/v1/licenses").unwrap_err();
        assert!(matches!(err, CliError::Network(_)));
    }

    #[test]
    fn invalid_json_body_on_success_status_is_invalid_response() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "not json", None)]));
        let c = client(t);
        let err = c.get("/v1/licenses").unwrap_err();
        assert!(matches!(err, CliError::InvalidResponse));
    }

    #[test]
    fn empty_success_body_resolves_to_json_null() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(204, "", None)]));
        let c = client(t);
        let outcome = c
            .call("DELETE", "/v1/integrations/stripe", None, None)
            .unwrap();
        match outcome {
            ApiOutcome::Ok { value, .. } => assert_eq!(value, serde_json::Value::Null),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
