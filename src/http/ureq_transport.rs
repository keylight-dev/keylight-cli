use super::{HttpResponse, Transport, TransportOutcome};

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        }
    }
}

/// Collapse a `ureq::Response` (whether it arrived as `Ok` or a non-2xx
/// `Status`) into our uniform `HttpResponse` so callers see the status code
/// consistently regardless of which `ureq` variant produced it.
fn parse_response(resp: ureq::Response) -> TransportOutcome {
    let status = resp.status();
    let retry_after = resp
        .header("Retry-After")
        .and_then(|h| h.parse::<u64>().ok());
    let notice = resp
        .header("Keylight-Notice")
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    let body = resp.into_string().unwrap_or_default();
    TransportOutcome::Response(HttpResponse {
        status,
        body,
        retry_after,
        notice,
    })
}

fn handle(res: Result<ureq::Response, ureq::Error>) -> TransportOutcome {
    match res {
        Ok(resp) => parse_response(resp),
        Err(ureq::Error::Status(_, resp)) => parse_response(resp),
        Err(ureq::Error::Transport(t)) => {
            use ureq::ErrorKind::*;
            match t.kind() {
                Io | Dns | ConnectionFailed => TransportOutcome::Transient(t.to_string()),
                _ => TransportOutcome::Terminal(t.to_string()),
            }
        }
    }
}

impl Transport for UreqTransport {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
    ) -> TransportOutcome {
        let mut req = self.agent.request(method, url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        handle(match body {
            Some(b) => req.send_string(b),
            None => req.call(),
        })
    }
}
