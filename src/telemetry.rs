//! How this CLI identifies itself to the management API.
//!
//! This is the *only* thing the CLI reports about itself, and it rides along on
//! requests it was already making — there is no beacon, no background call, and
//! nothing derived from the machine. That is deliberate: a fingerprint from an
//! open-source CLI is indefensible, and unnecessary here because every
//! meaningful action is already an authenticated call the server can see.
//!
//! Server-side this is the sole signal separating CLI traffic from agents and
//! raw `curl` against the same endpoints.

/// Crate version, resolved at compile time.
///
/// `env!` rather than a hand-maintained constant so it cannot drift from
/// `Cargo.toml` — the same failure the Swift SDK's literal `sdkVersion` is prone
/// to, which needs a release-time guard script to catch.
pub fn cli_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Short OS token.
///
/// Deliberately the same vocabulary as the Rust SDK's `platform()`
/// (`macos`/`linux`/`windows`/`unknown`) so CLI and SDK rows aggregate together
/// instead of needing a translation table. Note the JS SDK reports a *runtime*
/// taxonomy (`node`/`deno`/`web`) — a different axis, not a different spelling.
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// `keylight-cli/0.1.0 (macos)` — product/version plus platform, the
/// conventional User-Agent shape.
pub fn user_agent() -> String {
    format!("keylight-cli/{} ({})", cli_version(), platform())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_the_expected_shape() {
        let ua = user_agent();
        assert!(ua.starts_with("keylight-cli/"));
        assert!(ua.ends_with(')'));
        assert!(ua.contains(cli_version()));
        assert!(ua.contains(platform()));
    }

    /// Parsed server-side into client/version/platform columns; whitespace or a
    /// stray newline in the middle would break that split or, worse, forge a
    /// header boundary.
    #[test]
    fn user_agent_is_a_single_clean_header_line() {
        let ua = user_agent();
        assert!(!ua.contains('\n'));
        assert!(!ua.contains('\r'));
        assert_eq!(
            ua.split_whitespace().count(),
            2,
            "expected `product/ver (plat)`"
        );
    }

    #[test]
    fn platform_is_one_of_the_known_tokens() {
        assert!(matches!(
            platform(),
            "macos" | "linux" | "windows" | "unknown"
        ));
    }

    /// A version that fails to resolve would silently attribute every call to
    /// the wrong release.
    #[test]
    fn cli_version_is_populated() {
        assert!(!cli_version().is_empty());
        assert_eq!(cli_version(), env!("CARGO_PKG_VERSION"));
    }
}
