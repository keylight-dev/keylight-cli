//! Reading and writing the server-side `.env` that holds an API-mode app's
//! `licenses:runtime` token.
//!
//! The parsing/rewriting logic takes and returns strings — it never touches
//! the filesystem itself — so the merge rules (replace an existing key, keep
//! every other line, never duplicate) are unit-testable without a temp dir.
//! The command layer owns the actual read/write with `0600` permissions,
//! since the file holds a live bearer credential.

/// Upsert `KEY=value` into an existing `.env` body.
///
/// An existing assignment of the same key (an uncommented `KEY=...` line) is
/// replaced in place, preserving its position and every other line — a
/// developer's other variables and comments survive a re-run. A commented
/// `# KEY=...` line is left untouched and the real assignment is appended, so
/// re-running never silently edits something a human deliberately turned off.
/// When the key is absent it is appended with a trailing newline.
pub fn upsert(existing: &str, key: &str, value: &str) -> String {
    let assignment = format!("{key}={value}");
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();

    for line in existing.lines() {
        if !replaced && is_assignment_of(line, key) {
            out.push(assignment.clone());
            replaced = true;
        } else {
            out.push(line.to_string());
        }
    }

    if !replaced {
        out.push(assignment);
    }

    let mut body = out.join("\n");
    body.push('\n');
    body
}

/// True when `line` is an *uncommented* assignment of `key` (`KEY=...`, with
/// optional surrounding whitespace). A `# KEY=...` comment is not a live
/// assignment and must not be matched.
fn is_assignment_of(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    match trimmed.split_once('=') {
        Some((name, _)) => name.trim() == key,
        None => false,
    }
}

/// Whether `.gitignore` already ignores `entry` — an exact-line match, so a
/// broader glob a developer added themselves still counts as covered only if
/// it is literally that line. Conservative on purpose: a redundant entry is
/// harmless, a leaked `.env` is not.
pub fn is_gitignored(existing: &str, entry: &str) -> bool {
    existing.lines().any(|line| line.trim() == entry)
}

/// Append `entry` to a `.gitignore` body, keeping any existing content and
/// guaranteeing a trailing newline. Callers must gate this on
/// [`is_gitignored`] so the entry is never duplicated.
pub fn append_gitignore(existing: &str, entry: &str) -> String {
    let mut body = String::new();
    if !existing.is_empty() {
        body.push_str(existing);
        if !existing.ends_with('\n') {
            body.push('\n');
        }
    }
    body.push_str(entry);
    body.push('\n');
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_into_an_empty_file_writes_one_assignment() {
        assert_eq!(
            upsert("", "KEYLIGHT_API_TOKEN", "klm_abc"),
            "KEYLIGHT_API_TOKEN=klm_abc\n"
        );
    }

    #[test]
    fn upsert_appends_when_the_key_is_absent_and_keeps_the_rest() {
        let existing = "DATABASE_URL=postgres://x\nPORT=3000\n";
        let out = upsert(existing, "KEYLIGHT_API_TOKEN", "klm_abc");
        assert_eq!(
            out,
            "DATABASE_URL=postgres://x\nPORT=3000\nKEYLIGHT_API_TOKEN=klm_abc\n"
        );
    }

    /// A re-run must not stack a second token line — it replaces the existing
    /// assignment in place so the file has exactly one, wherever it already was.
    #[test]
    fn upsert_replaces_an_existing_assignment_in_place() {
        let existing = "A=1\nKEYLIGHT_API_TOKEN=klm_old\nB=2\n";
        let out = upsert(existing, "KEYLIGHT_API_TOKEN", "klm_new");
        assert_eq!(out, "A=1\nKEYLIGHT_API_TOKEN=klm_new\nB=2\n");
        assert_eq!(out.matches("KEYLIGHT_API_TOKEN=").count(), 1);
    }

    /// A commented-out assignment is a human's deliberate "off"; the real
    /// value is appended rather than silently re-enabling the comment.
    #[test]
    fn upsert_does_not_touch_a_commented_assignment() {
        let existing = "# KEYLIGHT_API_TOKEN=klm_disabled\n";
        let out = upsert(existing, "KEYLIGHT_API_TOKEN", "klm_new");
        assert_eq!(
            out,
            "# KEYLIGHT_API_TOKEN=klm_disabled\nKEYLIGHT_API_TOKEN=klm_new\n"
        );
    }

    #[test]
    fn gitignore_detects_an_exact_existing_entry() {
        assert!(is_gitignored("node_modules\n.env\n", ".env"));
        assert!(!is_gitignored("node_modules\n.env.local\n", ".env"));
    }

    #[test]
    fn gitignore_append_adds_a_trailing_newline_when_missing() {
        assert_eq!(
            append_gitignore("node_modules", ".env"),
            "node_modules\n.env\n"
        );
    }

    #[test]
    fn gitignore_append_to_empty_is_just_the_entry() {
        assert_eq!(append_gitignore("", ".env"), ".env\n");
    }
}
