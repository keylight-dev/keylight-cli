pub mod auth;
pub mod login;
pub mod logout;

use crate::config;
use std::path::PathBuf;

/// Everything a command needs, resolved once in `main` from CLI flags + env.
pub struct Context {
    pub json: bool,
    pub no_browser: bool,
    pub api_url: String,
    pub config_path: PathBuf,
}

impl Context {
    /// True when stdout is an interactive terminal — gates browser-opening
    /// (never pop a browser in a CI log or a piped script) alongside the
    /// explicit `--no-browser` flag.
    pub fn is_tty(&self) -> bool {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    }

    pub fn env_token(&self) -> Option<String> {
        std::env::var(config::ENV_TOKEN).ok()
    }
}
