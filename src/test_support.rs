//! Test doubles shared across modules. `#[cfg(test)]` only — never compiled
//! into the shipped binary.

use crate::http::{HttpResponse, Transport, TransportOutcome};
use std::sync::Mutex;

/// A recorded request: method, url, headers, body.
pub type RecordedRequest = (String, String, Vec<(String, String)>, Option<String>);

/// Returns queued responses in order and records every request it received,
/// so tests can assert on the exact wire calls made.
pub struct ScriptedTransport {
    responses: Mutex<Vec<TransportOutcome>>,
    pub requests: Mutex<Vec<RecordedRequest>>,
}

impl ScriptedTransport {
    pub fn new(responses: Vec<TransportOutcome>) -> Self {
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
        let mut r = self.responses.lock().unwrap();
        if r.is_empty() {
            panic!("ScriptedTransport ran out of queued responses");
        }
        r.remove(0)
    }
}

/// Build a canned JSON response. `retry_after` is only meaningful for 429s.
pub fn resp(status: u16, body: &str, retry_after: Option<u64>) -> TransportOutcome {
    TransportOutcome::Response(HttpResponse {
        status,
        body: body.to_string(),
        retry_after,
    })
}
