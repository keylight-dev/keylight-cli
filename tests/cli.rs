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
