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

/// Unix milliseconds → `YYYY-MM-DD` (UTC). Every timestamp the management API
/// returns — `createdAt`, `updatedAt`, `expiresAt` — is an integer of Unix ms,
/// not an ISO string, so a table column needs converting rather than
/// truncating. `--json` passes the raw number through for anyone who wants the
/// time or a different zone.
///
/// Hand-rolled rather than pulling in `chrono` for one function: the CLI has
/// four dependencies and a date crate would be the largest of them.
pub fn date_from_ms(value: Option<&serde_json::Value>) -> String {
    match value.and_then(|v| v.as_i64()) {
        Some(ms) => {
            let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
            format!("{y:04}-{m:02}-{d:02}")
        }
        // Non-numeric (including an explicit null `expiresAt`, which means a
        // perpetual license) falls through to the normal cell rendering.
        None => cell(value),
    }
}

/// Days since the Unix epoch → civil (year, month, day). Howard Hinnant's
/// `civil_from_days`, which is exact for the whole proleptic Gregorian range
/// and needs no lookup tables or leap-year special-casing.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
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
    fn unix_ms_renders_as_a_utc_date() {
        // 2026-06-18T09:31:02Z
        assert_eq!(
            date_from_ms(Some(&serde_json::json!(1_781_775_062_000i64))),
            "2026-06-18"
        );
    }

    /// A null `expiresAt` means a perpetual license, not a date — it must
    /// fall through to the normal dash rather than becoming the epoch.
    #[test]
    fn a_null_timestamp_is_a_dash_not_the_epoch() {
        assert_eq!(date_from_ms(Some(&serde_json::Value::Null)), "-");
        assert_eq!(date_from_ms(None), "-");
    }

    #[test]
    fn the_epoch_itself_renders_correctly() {
        assert_eq!(date_from_ms(Some(&serde_json::json!(0))), "1970-01-01");
    }

    /// Leap-year handling is the classic off-by-one in a hand-rolled date
    /// conversion, and 2000 is the century that *is* a leap year.
    #[test]
    fn leap_days_land_on_the_right_date() {
        // 2000-02-29T00:00:00Z and 2024-02-29T00:00:00Z
        assert_eq!(
            date_from_ms(Some(&serde_json::json!(951_782_400_000i64))),
            "2000-02-29"
        );
        assert_eq!(
            date_from_ms(Some(&serde_json::json!(1_709_164_800_000i64))),
            "2024-02-29"
        );
    }

    /// Timestamps before 1970 are negative; truncating division would round
    /// them toward zero and land on the wrong day.
    #[test]
    fn a_pre_epoch_timestamp_does_not_round_the_wrong_way() {
        // 1969-12-31T23:59:59Z — one second before the epoch.
        assert_eq!(date_from_ms(Some(&serde_json::json!(-1000))), "1969-12-31");
    }
}
