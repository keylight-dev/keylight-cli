//! Black-box CLI tests against the compiled binary (assert_cmd), same
//! pattern as the SDK repo's `keylight-cli/tests/cli.rs`. Every test
//! isolates `HOME`/`XDG_CONFIG_HOME` to a fresh tempdir so nothing here can
//! ever read or write a real user's `~/.config/keylight` or make a network
//! call with a real token.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn isolated_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("keylight").unwrap();
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("KEYLIGHT_API_TOKEN");
    cmd
}

/// `dirs::config_dir()` resolves differently per platform (and
/// `XDG_CONFIG_HOME` isn't honored on macOS), so rather than hardcode a
/// platform-specific path, find wherever `config.toml` actually landed
/// under the isolated home.
fn find_config_toml(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
                return Some(path);
            }
        }
    }
    None
}

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("keylight")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn no_subcommand_fails_with_usage() {
    Command::cargo_bin("keylight").unwrap().assert().failure();
}

#[test]
fn completions_and_man_stay_hidden_from_help() {
    let assert = Command::cargo_bin("keylight")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // The commands list is rendered one-per-line as `  <name>  <about>`; neither
    // packaging command should appear there.
    assert!(!stdout.contains("\n  completions"), "got: {stdout}");
    assert!(!stdout.contains("\n  man "), "got: {stdout}");
}

#[test]
fn completions_generate_a_shell_script_for_each_shell() {
    for (shell, needle) in [
        ("bash", "_keylight()"),
        ("zsh", "#compdef keylight"),
        ("fish", "complete -c keylight"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let assert = Command::cargo_bin("keylight")
            .unwrap()
            .args(["completions", shell])
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.contains(needle),
            "{shell} completions, got: {stdout}"
        );
    }
}

#[test]
fn man_renders_a_roff_page() {
    let assert = Command::cargo_bin("keylight")
        .unwrap()
        .arg("man")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(".TH keylight 1"), "got: {stdout}");
    assert!(stdout.contains(".SH NAME"), "got: {stdout}");
}

#[test]
fn logout_with_nothing_stored_makes_no_network_call_and_reports_it() {
    let home = tempfile::tempdir().unwrap();
    let assert = isolated_cmd(home.path())
        .args(["--json", "logout"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["removedLocalToken"], false);
    // Nothing to revoke, so nothing was revoked — and no request was made.
    // Both keys are part of the machine-readable contract.
    assert_eq!(json["revokedServerSide"], false);
}

#[test]
fn logout_with_nothing_stored_says_so_in_human_mode() {
    let home = tempfile::tempdir().unwrap();
    let assert = isolated_cmd(home.path()).arg("logout").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("No stored token"), "got: {stdout}");
}

#[test]
fn auth_status_without_any_token_fails_with_json_error_and_exit_code_2() {
    let home = tempfile::tempdir().unwrap();
    let assert = isolated_cmd(home.path())
        .args(["--json", "auth", "status"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(json["error"].as_str().unwrap().contains("no API token"));
}

#[test]
fn auth_status_without_any_token_human_mode_errors_on_stderr_not_stdout() {
    let home = tempfile::tempdir().unwrap();
    let assert = isolated_cmd(home.path())
        .args(["auth", "status"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.is_empty());
    assert!(stderr.contains("no API token"));
}

#[test]
fn auth_set_token_reads_stdin_and_persists_with_owner_only_permissions() {
    let home = tempfile::tempdir().unwrap();
    isolated_cmd(home.path())
        .args(["auth", "set-token"])
        .write_stdin("klm_test_token_123\n")
        .assert()
        .success();

    let config_path = find_config_toml(home.path()).expect("config.toml should have been written");
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("klm_test_token_123"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn auth_set_token_rejects_empty_stdin() {
    let home = tempfile::tempdir().unwrap();
    isolated_cmd(home.path())
        .args(["auth", "set-token"])
        .write_stdin("\n")
        .assert()
        .failure();
}

#[test]
fn env_token_is_used_without_ever_touching_the_config_file() {
    let home = tempfile::tempdir().unwrap();
    // No config file exists, only the env var — `auth status` must attempt
    // the network call (and thus fail with a network/DNS-shaped error, not
    // the "no API token configured" error) rather than falling back oddly.
    let assert = isolated_cmd(home.path())
        .env("KEYLIGHT_API_TOKEN", "klm_from_env")
        .env("KEYLIGHT_API_URL", "http://127.0.0.1:1") // nothing listens here
        .args(["--json", "auth", "status"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = json["error"].as_str().unwrap();
    assert!(
        !msg.contains("no API token"),
        "should have gotten past token resolution using the env var, got: {msg}"
    );
    assert!(find_config_toml(home.path()).is_none());
}

// ---------------------------------------------------------------------------
// Commercial settings: flag bounds are enforced before any network call, and
// a gated command walks the whole confirmation flow against a local mock.
// ---------------------------------------------------------------------------

#[test]
fn trial_set_rejects_days_outside_0_to_365_locally() {
    let home = tempfile::tempdir().unwrap();
    for days in ["-1", "366"] {
        let out = isolated_cmd(home.path())
            .env("KEYLIGHT_API_TOKEN", "klm_test")
            .env("KEYLIGHT_API_URL", "http://127.0.0.1:1")
            .args(["products", "trial", "set", "my-app", "--days", days])
            .assert()
            .failure();
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains("--days"),
            "days={days} should fail at flag parsing, got: {stderr}"
        );
    }
}

#[test]
fn set_price_rejects_amounts_below_the_minimum_locally() {
    let home = tempfile::tempdir().unwrap();
    let out = isolated_cmd(home.path())
        .env("KEYLIGHT_API_TOKEN", "klm_test")
        .env("KEYLIGHT_API_URL", "http://127.0.0.1:1")
        .args([
            "products",
            "key-types",
            "set-price",
            "pro",
            "--product",
            "my-app",
            "--price-in-cents",
            "49",
            "--currency",
            "usd",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("--price-in-cents"),
        "should fail at flag parsing, got: {stderr}"
    );
}

#[test]
fn gated_commercial_commands_say_so_in_their_help() {
    for args in [
        vec!["products", "trial", "set", "--help"],
        vec!["products", "free-tier", "set", "--help"],
        vec!["products", "key-types", "set-price", "--help"],
    ] {
        let out = Command::cargo_bin("keylight")
            .unwrap()
            .args(&args)
            .assert()
            .success();
        let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.contains("Needs approval in your browser"),
            "{args:?} help should mention the approval step, got: {stdout}"
        );
    }
}

/// One scripted reply per connection, in order.
fn serve_replies(
    replies: Vec<(u16, &'static str)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut request_lines = Vec::new();
        for (status, body) in replies {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap();
                }
            }
            let mut sent_body = vec![0; content_length];
            reader.read_exact(&mut sent_body).unwrap();
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request_lines.push(format!(
                "{} {}",
                request_line.trim(),
                String::from_utf8_lossy(&sent_body)
            ));
        }
        request_lines
    });
    (base, handle)
}

/// The agent contract end to end: with `--json --no-browser`, stdout carries
/// only the result, the approval URL goes to stderr, and the retried call is
/// the original one.
#[test]
fn trial_set_json_mode_walks_the_flow_with_url_on_stderr_only() {
    let home = tempfile::tempdir().unwrap();
    let (base, server) = serve_replies(vec![
        (
            202,
            r#"{"confirmation":{"id":"c1","status":"pending","approveUrl":"https://app.keylight.dev/c/c1","expiresInSeconds":600}}"#,
        ),
        (200, r#"{"id":"c1","status":"approved"}"#),
        (
            200,
            r#"{"product":{"productId":"my-app","configuredTrialDurationDays":14}}"#,
        ),
    ]);
    let out = isolated_cmd(home.path())
        .env("KEYLIGHT_API_TOKEN", "klm_test")
        .args([
            "--json",
            "--no-browser",
            "--api-url",
            &base,
            "products",
            "trial",
            "set",
            "my-app",
            "--days",
            "14",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout must be only the JSON result: {e}\n{stdout}");
    });
    assert_eq!(parsed["product"]["configuredTrialDurationDays"], 14);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("Approve it at: https://app.keylight.dev/c/c1"),
        "got: {stderr}"
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("POST /v1/products/my-app/trial-duration"));
    assert!(requests[0].ends_with(r#"{"days":14}"#));
    assert!(requests[1].starts_with("GET /v1/confirmations/c1"));
    assert!(requests[2].starts_with("POST /v1/products/my-app/trial-duration"));
}
