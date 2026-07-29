//! `--json` vs human output. Every command builds one `serde_json::Value`
//! representing its result and hands it to `emit`, which either prints the
//! raw JSON (machine-readable — the contract agents/CI script against) or a
//! command-supplied human rendering.
//!
//! The human renderers here are deliberately small and dependency-free. A
//! table crate (`comfy-table` and friends) was rejected: the CLI has four
//! dependencies today, and `table` below is the only place one is needed.

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

/// Column widths come from the longest cell, including the header. Two-space
/// gutter, no borders, no wrapping — wrapping would break `awk`/`cut` on the
/// output, and a terminal that is too narrow is the user's to resize.
///
/// Trailing whitespace is trimmed off each line so the last column doesn't
/// carry invisible padding into a diff or a copy-paste.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    println!(
        "{}",
        pad_row(
            &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
            &widths
        )
    );
    for row in rows {
        println!("{}", pad_row(row, &widths));
    }
}

fn pad_row(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        line.push_str(cell);
        // Pad every column but the last, whose padding would be trailing
        // whitespace.
        if i + 1 < cells.len() {
            let w = widths.get(i).copied().unwrap_or(0);
            for _ in cell.chars().count()..w {
                line.push(' ');
            }
        }
    }
    line.trim_end().to_string()
}

/// Key/value block for a single item — a row that wide is unreadable as a
/// table. Labels are right-padded so the values line up.
pub fn kv(pairs: &[(&str, String)]) {
    let width = pairs
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    for (k, v) in pairs {
        println!("{k}:{:pad$} {v}", "", pad = width - k.chars().count());
    }
}

/// An empty result always carries the next action — "no products" without
/// "here is how you make one" is a dead end.
pub fn empty(message: &str) {
    println!("{message}");
}

/// Render a JSON value as a display string for a table cell. Strings lose
/// their quotes; null and missing both become `-`, since a column of `null`
/// is noise.
pub fn cell(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => "-".to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Array(a)) => a.len().to_string(),
        Some(other) => other.to_string(),
    }
}

/// `2026-06-18T09:31:02.000Z` → `2026-06-18`. Table columns show the date;
/// `--json` keeps the full timestamp for anyone who needs it.
pub fn date_only(value: Option<&serde_json::Value>) -> String {
    let s = cell(value);
    match s.split_once('T') {
        Some((date, _)) => date.to_string(),
        None => s,
    }
}

/// Print the cursor that continues a list, when the server sent one.
/// Pagination is explicit and never automatic (design spec), so this line is
/// the only thing telling a human the list was truncated.
pub fn next_cursor_hint(value: &serde_json::Value, command: &str) {
    if let Some(c) = value.get("nextCursor").and_then(|c| c.as_str()) {
        println!();
        println!("More results. Continue with: {command} --cursor {c}");
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

    /// Column width is driven by the longest cell, not the header, so a value
    /// wider than its header still lines up.
    #[test]
    fn row_padding_widens_to_the_longest_cell() {
        let widths = vec![8, 4];
        let line = pad_row(&["my-app".into(), "3".into()], &widths);
        assert_eq!(line, "my-app    3");
    }

    /// The last column is never padded — trailing whitespace survives into
    /// copy-paste and shows up as a diff artifact.
    #[test]
    fn last_column_carries_no_trailing_whitespace() {
        let widths = vec![4, 20];
        let line = pad_row(&["id".into(), "short".into()], &widths);
        assert_eq!(line, "id    short");
        assert!(!line.ends_with(' '));
    }

    /// Width is counted in characters, not bytes: a multi-byte name must not
    /// shift the following column.
    #[test]
    fn column_width_counts_characters_not_bytes() {
        let widths = vec![6, 4];
        let ascii = pad_row(&["abcdef".into(), "x".into()], &widths);
        let accented = pad_row(&["éèêëîï".into(), "x".into()], &widths);
        // Character position, not byte offset — the accented row is 6 chars
        // but 12 bytes, so comparing `find` would pass for the wrong reason.
        assert_eq!(
            ascii.chars().position(|c| c == 'x'),
            accented.chars().position(|c| c == 'x'),
            "both rows are 6 characters wide, so `x` starts at the same column"
        );
    }

    #[test]
    fn null_and_missing_cells_both_render_as_a_dash() {
        assert_eq!(cell(None), "-");
        assert_eq!(cell(Some(&serde_json::Value::Null)), "-");
    }

    #[test]
    fn string_cells_lose_their_json_quotes() {
        assert_eq!(cell(Some(&serde_json::json!("my-app"))), "my-app");
    }

    /// An array renders as its length — a table cell holding a whole JSON
    /// array is unreadable, and the count is what a list view wants.
    #[test]
    fn array_cells_render_as_a_count() {
        assert_eq!(cell(Some(&serde_json::json!(["a", "b", "c"]))), "3");
    }

    #[test]
    fn timestamps_are_truncated_to_the_date() {
        assert_eq!(
            date_only(Some(&serde_json::json!("2026-06-18T09:31:02.000Z"))),
            "2026-06-18"
        );
    }

    /// A value that isn't a timestamp passes through rather than being
    /// mangled by the split.
    #[test]
    fn a_non_timestamp_passes_through_date_only_untouched() {
        assert_eq!(date_only(Some(&serde_json::json!("never"))), "never");
        assert_eq!(date_only(None), "-");
    }
}
