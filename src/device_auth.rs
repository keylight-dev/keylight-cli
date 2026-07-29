//! RFC 8628 device authorization — the protocol half of `keylight login`.
//!
//! Deliberately knows nothing about `Context`, config files, or printing, so it
//! can be driven entirely by `ScriptedTransport` in tests. The command layer in
//! `commands/login.rs` owns all of that.

use crate::api::{ApiClient, ApiOutcome};
use crate::error::{CliError, Result};
use serde::Deserialize;

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
}
