//! Advisory project detection for `keylight init`.
//!
//! Given the file/directory names in the project root, guess whether an API or
//! SDK integration fits, and whether the project looks like something that
//! ships to an end-user's machine (a browser or native client) where a
//! `licenses:runtime` bearer token must never be embedded.
//!
//! This is **advice, never a gate** (design §F4/§F5): the recommendation is
//! shown, the user always chooses. The logic takes a plain list of names so
//! every rule is unit-testable without a real directory.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sdk,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    pub recommended: Mode,
    /// One line explaining the recommendation, shown to the user.
    pub reason: String,
    /// The project looks like it ships to an untrusted client (a browser bundle
    /// or a native binary a customer runs). A `klm_` runtime token embedded
    /// there is readable by anyone who has the app, so API mode is the wrong
    /// tool — steer to an SDK. Drives the §F5 guard-rail warning.
    pub client_risk: bool,
}

/// Case-insensitive "is one of these names present".
fn has(files: &[&str], name: &str) -> bool {
    files.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// Case-insensitive "is there a name ending with this suffix" — for Xcode
/// project bundles and .NET project files, whose stem is the app's name.
fn has_suffix(files: &[&str], suffix: &str) -> bool {
    files
        .iter()
        .any(|f| f.to_ascii_lowercase().ends_with(suffix))
}

/// Native desktop / mobile markers — a Keylight SDK verifies signed licenses
/// on the device and works offline, so these want SDK mode.
fn is_native(files: &[&str]) -> bool {
    has(files, "src-tauri")
        || has(files, "tauri.conf.json")
        || has(files, "Package.swift")
        || has(files, "Cargo.toml")
        || has(files, "CMakeLists.txt")
        || has_suffix(files, ".xcodeproj")
        || has_suffix(files, ".xcworkspace")
        || has_suffix(files, ".csproj")
}

/// Browser / client-only markers — a static front-end bundle. It ships to the
/// user's browser, so it must not carry a runtime token; recommend an SDK and
/// raise the client-risk flag.
fn is_browser(files: &[&str]) -> bool {
    has(files, "index.html")
        || has(files, "vite.config.js")
        || has(files, "vite.config.ts")
        || has(files, "svelte.config.js")
}

/// Next.js / Node server markers — a server-side runtime that can hold a secret
/// and speak plain HTTPS, which is exactly API mode's home.
fn is_server_js(files: &[&str]) -> bool {
    has(files, "next.config.js")
        || has(files, "next.config.ts")
        || has(files, "next.config.mjs")
        || has(files, "server.js")
        || has(files, "server.ts")
}

/// Turn a project-root listing into a recommendation.
///
/// Precedence is most-specific first: a native marker wins over everything (a
/// Tauri app with a `package.json` is still native); an explicit server
/// framework marks API; a browser bundle marks SDK-with-risk; a bare
/// `package.json` (Node, server-capable) leans API; anything else defaults to
/// the safe SDK recommendation.
pub fn detect_from_files(files: &[&str]) -> Advice {
    if is_native(files) {
        return Advice {
            recommended: Mode::Sdk,
            reason: "looks like a native desktop or mobile app — an SDK verifies \
                     licenses on the device and works offline"
                .into(),
            client_risk: false,
        };
    }

    if is_server_js(files) {
        return Advice {
            recommended: Mode::Api,
            reason: "looks like a Next.js / Node server — call the HTTP API \
                     directly, keeping the token server-side"
                .into(),
            client_risk: false,
        };
    }

    if is_browser(files) {
        return Advice {
            recommended: Mode::Sdk,
            reason: "looks like a browser / client-only app — never embed a \
                     runtime token in code you ship to a browser; use an SDK"
                .into(),
            client_risk: true,
        };
    }

    if has(files, "package.json") {
        return Advice {
            recommended: Mode::Api,
            reason: "looks like a Node project — if this is a server, call the \
                     HTTP API directly with a server-side token"
                .into(),
            client_risk: false,
        };
    }

    Advice {
        recommended: Mode::Sdk,
        reason: "couldn't tell what kind of project this is — defaulting to the \
                 SDK, which is safe for any client. Choose API only for a \
                 server you control."
            .into(),
        client_risk: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextjs_is_recommended_the_api() {
        let a = detect_from_files(&["package.json", "next.config.js", "pages"]);
        assert_eq!(a.recommended, Mode::Api);
        assert!(!a.client_risk);
    }

    #[test]
    fn a_tauri_app_is_recommended_the_sdk_even_with_a_package_json() {
        // The native marker must win over the Node one — a Tauri app is native.
        let a = detect_from_files(&["package.json", "src-tauri", "vite.config.ts"]);
        assert_eq!(a.recommended, Mode::Sdk);
        assert!(!a.client_risk, "a native app is not a browser-embed risk");
    }

    #[test]
    fn an_xcode_project_is_native_sdk() {
        assert_eq!(
            detect_from_files(&["MyApp.xcodeproj", "MyApp"]).recommended,
            Mode::Sdk
        );
        assert_eq!(
            detect_from_files(&["Package.swift", "Sources"]).recommended,
            Mode::Sdk
        );
    }

    #[test]
    fn a_browser_spa_is_steered_to_the_sdk_with_a_client_risk_flag() {
        let a = detect_from_files(&["package.json", "index.html", "vite.config.ts"]);
        assert_eq!(a.recommended, Mode::Sdk);
        assert!(
            a.client_risk,
            "a browser bundle must raise the client-risk flag so init warns \
             against embedding the token"
        );
    }

    #[test]
    fn a_bare_node_project_leans_api() {
        let a = detect_from_files(&["package.json", "index.js"]);
        assert_eq!(a.recommended, Mode::Api);
        assert!(!a.client_risk);
    }

    #[test]
    fn an_unrecognized_project_defaults_to_the_sdk() {
        let a = detect_from_files(&["README.md", "LICENSE"]);
        assert_eq!(a.recommended, Mode::Sdk);
        assert!(!a.client_risk);
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(
            detect_from_files(&["Cargo.toml"]).recommended,
            detect_from_files(&["cargo.toml"]).recommended
        );
    }
}
