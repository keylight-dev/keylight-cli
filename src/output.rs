//! `--json` vs human output. Every command builds one `serde_json::Value`
//! representing its result and hands it to `emit`, which either prints the
//! raw JSON (machine-readable — the contract agents/CI script against) or a
//! command-supplied human rendering.

pub fn emit_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

/// Print `value` as pretty JSON if `json` is set, otherwise call `human` to
/// render it as text/tables. Centralizes the branch so every command has an
/// identical `--json` contract instead of reimplementing the `if`.
pub fn emit(json: bool, value: &serde_json::Value, human: impl FnOnce(&serde_json::Value)) {
    if json {
        emit_json(value);
    } else {
        human(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_true_skips_the_human_renderer() {
        let mut human_called = false;
        emit(true, &serde_json::json!({"a": 1}), |_| human_called = true);
        assert!(!human_called);
    }

    #[test]
    fn json_false_invokes_the_human_renderer() {
        let mut human_called = false;
        emit(false, &serde_json::json!({"a": 1}), |_| human_called = true);
        assert!(human_called);
    }
}
