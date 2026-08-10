//! Auto step-up for destructive commands: when the API rejects a call with
//! `403 Missing scope: danger`, re-authorize for `danger` through the existing
//! device flow, then let the caller retry. npm-style: page 1 grants the
//! capability, page 2 (the existing confirmation) approves the specific action.

use super::{Context, Deps};
use crate::api::ApiOutcome;
use crate::config::{self, ConfigFile};
use crate::device_auth::{self, PollOutcome};
use crate::error::{CliError, Result};

/// The token's current scopes with `danger` appended when missing.
///
/// The device flow treats an explicit scope list as *exact*, so re-authorizing
/// for `danger` alone would silently drop every capability the token already
/// had. We request the union instead. Order is preserved so the grant page
/// lists permissions in the same order the user last saw them.
fn scopes_with_danger(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|s| s == "danger") {
        out.push("danger".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_danger_when_absent() {
        let got = scopes_with_danger(&["licenses:read".to_string(), "licenses:write".to_string()]);
        assert_eq!(got, vec!["licenses:read", "licenses:write", "danger"]);
    }

    #[test]
    fn leaves_scopes_untouched_when_danger_already_present() {
        let got = scopes_with_danger(&["licenses:write".to_string(), "danger".to_string()]);
        assert_eq!(got, vec!["licenses:write", "danger"]);
    }

    #[test]
    fn appends_danger_to_an_empty_scope_set() {
        let got = scopes_with_danger(&[]);
        assert_eq!(got, vec!["danger"]);
    }
}
