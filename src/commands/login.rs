//! `keylight login` — browser-based auth.
//!
//! **Status: this implementation targets a design that was not the one
//! shipped. It does not work.** `keylight auth set-token` is the working
//! path today.
//!
//! What is below is the *loopback callback* flow: bind a localhost listener
//! on an ephemeral port, open the browser to a dashboard grant page with
//! `?cli_callback=http://127.0.0.1:<port>/callback`, and wait for that page
//! to redirect back with `?token=klm_...`. Nothing serves that contract, so
//! it will wait out [`CALLBACK_TIMEOUT`] and fail.
//!
//! What actually shipped (and is live) is **RFC 8628 device authorization**,
//! chosen over loopback because the CLI has to work on headless boxes and
//! over SSH, where a browser cannot reach the CLI's localhost:
//!
//! 1. `POST /v1/login/device` `{scopes}` → `{device_code, user_code,
//!    verification_uri, interval, expires_in}`
//! 2. Show the user `user_code`; open `verification_uri`
//!    (`https://app.keylight.dev/login/device`) for them to approve
//! 3. Poll `POST /v1/login/device/token` `{device_code}` every `interval`
//!    seconds → `{"status":"pending"}` until granted, then the token ONCE
//! 4. Persist via [`crate::config`], same as `auth set-token`
//!
//! Rewriting this module against those three endpoints is the remaining
//! work. Note the loopback listener, port handling and browser-opening below
//! are NOT reusable for it — the device flow needs no local listener at all.
//! Keep [`Opener`] and the config write; drop the rest.

use super::Context;
use crate::config::{self, ConfigFile};
use crate::confirmation::{Opener, RealOpener};
use crate::error::{CliError, Result};
use crate::output::emit;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Nothing will hit the callback before this elapses today (see module
/// docs); once the grant page exists this is simply "how long we'll wait
/// for a human to click Approve".
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// The one thing that needs to change once the dashboard grant page ships:
/// where it lives and what query param it reads for the callback URL.
pub fn grant_url(port: u16) -> String {
    let callback = format!("http://127.0.0.1:{port}/callback");
    format!(
        "https://app.keylight.dev/dashboard?cli_callback={}",
        urlencode(&callback)
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallbackOutcome {
    Token(String),
    Error(String),
}

/// `request_target` is the second token of an HTTP request line, e.g.
/// `/callback?token=klm_abc`. Returns `None` for any path other than
/// `/callback` (a stray favicon request, browser prefetch, etc.) so the
/// listener keeps waiting instead of treating it as the real callback.
fn parse_callback(request_target: &str) -> Option<CallbackOutcome> {
    let (path, query) = request_target
        .split_once('?')
        .unwrap_or((request_target, ""));
    if path != "/callback" {
        return None;
    }
    let params = parse_query(query);
    if let Some(t) = params.get("token") {
        return Some(CallbackOutcome::Token(t.clone()));
    }
    if let Some(e) = params.get("error") {
        return Some(CallbackOutcome::Error(e.clone()));
    }
    Some(CallbackOutcome::Error(
        "callback received with neither a token nor an error".into(),
    ))
}

fn request_target(request_line: &str) -> Option<&str> {
    request_line.split_whitespace().nth(1)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())
}

/// Block on `listener` for exactly one `/callback` request (ignoring
/// anything else, e.g. a stray favicon probe), up to `timeout`. Split out
/// from [`login`] so the accept/parse/respond loop is testable against a
/// real local socket without a browser or the dashboard involved.
pub fn wait_for_callback(listener: TcpListener, timeout: Duration) -> Result<CallbackOutcome> {
    let (tx, rx) = std::sync::mpsc::channel::<CallbackOutcome>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = match stream.try_clone() {
                Ok(s) => BufReader::new(s),
                Err(_) => continue,
            };
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let Some(target) = request_target(&line) else {
                continue;
            };
            match parse_callback(target) {
                Some(outcome) => {
                    let body = match &outcome {
                        CallbackOutcome::Token(_) => {
                            "You're authenticated. You can close this tab and return to the terminal."
                        }
                        CallbackOutcome::Error(_) => {
                            "Authorization failed. You can close this tab and return to the terminal."
                        }
                    };
                    let _ = write_response(&mut stream, 200, "OK", body);
                    let _ = tx.send(outcome);
                    return;
                }
                None => {
                    let _ = write_response(&mut stream, 404, "Not Found", "");
                }
            }
        }
    });

    rx.recv_timeout(timeout).map_err(|_| {
        CliError::Config(
            "timed out waiting for the browser callback. The dashboard grant page isn't live \
             yet — use `keylight auth set-token` instead (create a token from Settings > API \
             tokens on the dashboard, then paste it in)."
                .into(),
        )
    })
}

pub fn login(ctx: &Context) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = grant_url(port);

    println!("Opening your browser to authorize this CLI:");
    println!("  {url}");
    println!("Waiting for approval (Ctrl+C to cancel)...");
    if !ctx.no_browser && ctx.is_tty() {
        RealOpener.open(&url);
    }

    match wait_for_callback(listener, CALLBACK_TIMEOUT)? {
        CallbackOutcome::Token(token) => {
            config::write_config(&ctx.config_path, &ConfigFile { token: Some(token) })?;
            let result = serde_json::json!({ "authenticated": true });
            emit(ctx.json, &result, |_| {
                println!("Logged in. Token stored at {}.", ctx.config_path.display());
            });
            Ok(())
        }
        CallbackOutcome::Error(msg) => Err(CliError::Config(format!("login failed: {msg}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_url_percent_encodes_the_callback() {
        let url = grant_url(54321);
        assert_eq!(
            url,
            "https://app.keylight.dev/dashboard?cli_callback=http%3A%2F%2F127.0.0.1%3A54321%2Fcallback"
        );
    }

    #[test]
    fn parse_callback_extracts_token() {
        assert_eq!(
            parse_callback("/callback?token=klm_abc123"),
            Some(CallbackOutcome::Token("klm_abc123".into()))
        );
    }

    #[test]
    fn parse_callback_extracts_error() {
        assert_eq!(
            parse_callback("/callback?error=access_denied"),
            Some(CallbackOutcome::Error("access_denied".into()))
        );
    }

    #[test]
    fn parse_callback_ignores_unrelated_paths() {
        assert_eq!(parse_callback("/favicon.ico"), None);
    }

    #[test]
    fn parse_callback_with_neither_param_is_an_error_outcome() {
        assert_eq!(
            parse_callback("/callback"),
            Some(CallbackOutcome::Error(
                "callback received with neither a token nor an error".into()
            ))
        );
    }

    #[test]
    fn percent_decode_handles_encoded_bytes_and_plus() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a%3Db"), "a=b");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn wait_for_callback_returns_token_from_a_real_local_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            std::thread::spawn(move || wait_for_callback(listener, Duration::from_secs(5)));

        // Give the accept loop a moment to start listening, then send a raw
        // HTTP GET the way a browser redirect would.
        std::thread::sleep(Duration::from_millis(50));
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /callback?token=klm_from_test HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();

        let outcome = handle.join().unwrap().unwrap();
        assert_eq!(outcome, CallbackOutcome::Token("klm_from_test".into()));
    }

    #[test]
    fn wait_for_callback_ignores_unrelated_requests_then_accepts_the_real_one() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            std::thread::spawn(move || wait_for_callback(listener, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));

        let mut noise = TcpStream::connect(("127.0.0.1", port)).unwrap();
        noise
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        drop(noise);

        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        real.write_all(b"GET /callback?token=klm_second HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();

        let outcome = handle.join().unwrap().unwrap();
        assert_eq!(outcome, CallbackOutcome::Token("klm_second".into()));
    }

    #[test]
    fn wait_for_callback_times_out_when_nothing_connects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let err = wait_for_callback(listener, Duration::from_millis(100)).unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
    }
}
