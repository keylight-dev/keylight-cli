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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The same outcome with the success body left as received. Only
/// `GET /v1/licenses/export` needs this — it answers `text/csv`, which the
/// JSON parse in [`ApiClient::call`] would reject as `InvalidResponse`.
#[derive(Debug)]
pub enum RawOutcome {
    Ok(String),
    Pending(PendingConfirmation),
}

/// A request that reached a verdict: either a success body (whatever its
/// content type) or a pending confirmation. Retries, the `{"error": ...}`
/// envelope and rate limiting are already resolved by the time one exists,
/// so the JSON and raw entry points share all of that rather than forking it.
enum Settled {
    Body {
        body: String,
        retry_after: Option<u64>,
    },
    Pending(PendingConfirmation),
}

fn print_notice(notice: &str) {
    eprintln!("warning: {notice}");
}

/// The server's `Keylight-Notice`, shown at most once per command. One is made
/// per command invocation (on [`crate::commands::Deps`]) and shared by every
/// client that command builds — a command may build several (setup, the
/// step-up retry) and each would otherwise print it again.
pub struct NoticeOnce {
    shown: AtomicBool,
    /// Where the notice goes. Stderr in production, so a notice never lands
    /// in `--json` or CSV on stdout.
    sink: Box<dyn Fn(&str) + Send + Sync>,
}

impl Default for NoticeOnce {
    fn default() -> Self {
        Self::with_sink(print_notice)
    }
}

impl NoticeOnce {
    pub fn with_sink(sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            shown: AtomicBool::new(false),
            sink: Box::new(sink),
        }
    }

    fn show(&self, notice: Option<&str>) {
        if let Some(n) = notice
            && !self.shown.swap(true, Ordering::Relaxed)
        {
            (self.sink)(n);
        }
    }
}

pub struct ApiClient {
    transport: Arc<dyn Transport>,
    sleeper: Arc<dyn Sleeper>,
    base_url: String,
    token: Option<String>,
    notice: Arc<NoticeOnce>,
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
            token: Some(token.into()),
            notice: Arc::new(NoticeOnce::default()),
        }
    }

    /// A client for the endpoints that take no auth — the device-login start
    /// and poll pair. Sending an empty bearer token instead would work, but it
    /// would also make a genuinely missing token indistinguishable from a
    /// deliberately unauthenticated call.
    pub fn anonymous(
        transport: Arc<dyn Transport>,
        sleeper: Arc<dyn Sleeper>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            sleeper,
            base_url: base_url.into(),
            token: None,
            notice: Arc::new(NoticeOnce::default()),
        }
    }

    /// Share the command's notice state, so the notice shows once per command
    /// however many clients and requests the command makes. A client built
    /// without this shows it once for itself.
    pub fn sharing_notice(mut self, notice: Arc<NoticeOnce>) -> Self {
        self.notice = notice;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn headers(&self, confirmation_id: Option<&str>) -> Vec<(String, String)> {
        let mut h = vec![
            ("Content-Type".into(), "application/json".into()),
            // The only thing distinguishing CLI traffic from an agent or raw
            // curl on the same endpoints. See crate::telemetry.
            ("User-Agent".into(), crate::telemetry::user_agent()),
        ];
        if let Some(token) = &self.token {
            h.push(("Authorization".into(), format!("Bearer {token}")));
        }
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
        self.call_with_headers(method, path, body, confirmation_id, &[])
    }

    /// As [`ApiClient::call`], with additional request headers.
    ///
    /// `POST /v1/licenses` and `POST /v1/licenses/import` accept an
    /// `Idempotency-Key`, which matters because a retried mint that the
    /// server already processed would otherwise issue a second license to the
    /// same customer.
    pub fn call_with_headers(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        confirmation_id: Option<&str>,
        extra: &[(String, String)],
    ) -> Result<ApiOutcome> {
        match self.settle_with(method, path, body, confirmation_id, extra)? {
            Settled::Pending(p) => Ok(ApiOutcome::Pending(p)),
            Settled::Body { body, retry_after } => {
                let value = if body.trim().is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_str(&body).map_err(|_| CliError::InvalidResponse)?
                };
                Ok(ApiOutcome::Ok { value, retry_after })
            }
        }
    }

    /// As [`ApiClient::call`], but hands back the success body verbatim
    /// instead of parsing it. For `text/csv` responses, where a JSON parse
    /// would be wrong rather than merely wasteful.
    pub fn call_raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        confirmation_id: Option<&str>,
    ) -> Result<RawOutcome> {
        match self.settle(method, path, body, confirmation_id)? {
            Settled::Pending(p) => Ok(RawOutcome::Pending(p)),
            Settled::Body { body, .. } => Ok(RawOutcome::Ok(body)),
        }
    }

    fn settle(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        confirmation_id: Option<&str>,
    ) -> Result<Settled> {
        self.settle_with(method, path, body, confirmation_id, &[])
    }

    fn settle_with(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        confirmation_id: Option<&str>,
        extra: &[(String, String)],
    ) -> Result<Settled> {
        let url = self.url(path);
        let mut headers = self.headers(confirmation_id);
        headers.extend_from_slice(extra);
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
    ) -> Result<Settled> {
        if let TransportOutcome::Response(r) = &outcome {
            self.notice.show(r.notice.as_deref());
        }
        match outcome {
            TransportOutcome::Response(r) if r.status == 202 => {
                let env: ConfirmationEnvelope =
                    serde_json::from_str(&r.body).map_err(|_| CliError::InvalidResponse)?;
                Ok(Settled::Pending(env.confirmation))
            }
            TransportOutcome::Response(r) if (200..300).contains(&r.status) => Ok(Settled::Body {
                body: r.body,
                retry_after: r.retry_after,
            }),
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
    use crate::sleep::NoopSleeper;
    use crate::test_support::{ScriptedTransport, resp, resp_with_notice};

    fn client(transport: Arc<ScriptedTransport>) -> ApiClient {
        ApiClient::new(
            transport,
            Arc::new(NoopSleeper::default()),
            "https://api.keylight.dev",
            "klm_test",
        )
    }

    fn client_with_sink(
        transport: Arc<ScriptedTransport>,
    ) -> (ApiClient, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let c = client(transport).sharing_notice(Arc::new(NoticeOnce::with_sink(move |n| {
            sink.lock().unwrap().push(n.to_string())
        })));
        (c, seen)
    }

    const NOTICE: &str = "Two-step sign-in becomes required on this account on 2026-11-29. Set it up at app.keylight.dev.";

    #[test]
    fn notice_header_is_shown_once_across_several_requests() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp_with_notice(200, "{}", NOTICE),
            resp_with_notice(200, "{}", NOTICE),
        ]));
        let (c, seen) = client_with_sink(t);
        c.get("/v1/licenses").unwrap();
        c.get("/v1/licenses").unwrap();
        assert_eq!(seen.lock().unwrap().as_slice(), [NOTICE]);
    }

    #[test]
    fn no_notice_header_prints_nothing() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let (c, seen) = client_with_sink(t);
        c.get("/v1/licenses").unwrap();
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn notice_is_shown_even_when_the_response_is_an_error() {
        let t = Arc::new(ScriptedTransport::new(vec![resp_with_notice(
            404,
            r#"{"error":"Not found"}"#,
            NOTICE,
        )]));
        let (c, seen) = client_with_sink(t);
        assert!(c.get("/v1/products/x").is_err());
        assert_eq!(seen.lock().unwrap().as_slice(), [NOTICE]);
    }

    #[test]
    fn two_step_required_403_message_is_kept_verbatim() {
        let msg = "Two-step sign-in is required on this Keylight account. Set it up at app.keylight.dev, then run `keylight login` again.";
        let body = serde_json::json!({ "error": msg }).to_string();
        let t = Arc::new(ScriptedTransport::new(vec![resp(403, &body, None)]));
        let err = client(t).get("/v1/licenses").unwrap_err();
        assert!(err.to_string().contains(msg), "got: {err}");
        match err {
            CliError::Api { status, message } => {
                assert_eq!(status, 403);
                assert_eq!(message, msg);
            }
            other => panic!("expected Api error, got {other:?}"),
        }
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

    /// The server segments API traffic by client (CLI vs. agent vs. raw curl)
    /// purely from this header — there is no other signal, and the CLI sends no
    /// separate telemetry beacon. If it goes missing, CLI usage silently becomes
    /// indistinguishable from anyone else hitting /v1.
    #[test]
    fn user_agent_header_is_sent() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let c = client(t.clone());
        c.get("/v1/tokens/verify").unwrap();
        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];

        let ua = headers
            .iter()
            .find(|(k, _)| k == "User-Agent")
            .map(|(_, v)| v.clone())
            .expect("User-Agent must be sent on every request");

        assert!(ua.starts_with("keylight-cli/"), "unexpected shape: {ua}");
        assert!(
            ua.contains(env!("CARGO_PKG_VERSION")),
            "must carry the crate version: {ua}"
        );
    }

    /// The retry leg reuses the header vec, so this guards against a future
    /// refactor that rebuilds headers for the retry and drops the identity.
    /// 429 rather than 5xx because that is the only status this client retries
    /// — the management API's limiter is a per-minute token bucket, so the
    /// policy is one wait-for-`Retry-After` retry, not a backoff loop.
    #[test]
    fn user_agent_survives_a_retry() {
        let t = Arc::new(ScriptedTransport::new(vec![
            resp(429, "{}", Some(0)),
            resp(200, "{}", None),
        ]));
        let c = client(t.clone());
        c.get("/v1/tokens/verify").unwrap();
        let reqs = t.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2, "expected an initial call plus one retry");
        for (_, _, headers, _) in reqs.iter() {
            assert!(
                headers.iter().any(|(k, _)| k == "User-Agent"),
                "every attempt must carry the User-Agent"
            );
        }
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

    #[test]
    fn anonymous_client_sends_no_authorization_header() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        c.get("/v1/login/device").unwrap();
        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        assert!(
            !headers.iter().any(|(k, _)| k == "Authorization"),
            "anonymous client must not send Authorization"
        );
    }

    #[test]
    fn anonymous_client_still_sends_user_agent() {
        let t = Arc::new(ScriptedTransport::new(vec![resp(200, "{}", None)]));
        let c = ApiClient::anonymous(
            t.clone(),
            Arc::new(NoopSleeper::default()),
            "https://api.test",
        );
        c.get("/v1/login/device").unwrap();
        let reqs = t.requests.lock().unwrap();
        let (_, _, headers, _) = &reqs[0];
        assert!(headers.iter().any(|(k, _)| k == "User-Agent"));
    }
}
