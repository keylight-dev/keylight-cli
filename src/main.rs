//! `keylight` — the Keylight management CLI. Thin client over
//! `https://api.keylight.dev/v1`, the same API the tenant dashboard is
//! built on (design: docs/superpowers/specs/2026-07-23-management-api-cli-design.md
//! in the main `keylight` repo).
//!
//! Every endpoint the management API exposes is reachable from a command;
//! `tests/coverage.rs` fails the build if that stops being true.
//!
//! Commands are hand-written rather than generated from the OpenAPI spec.
//! Help text, flag names, output formatting and error messages are the entire
//! value over `curl`, and a generated CLI reads like `curl` with extra steps.
//! Drift protection comes from the coverage test instead, which catches the
//! failure that actually matters — an endpoint no command reaches.

use clap::{Parser, Subcommand};
use keylight_cli::commands::{self, Context, Deps};
use keylight_cli::config;
use keylight_cli::error::CliError;
use std::io::IsTerminal;

#[derive(Parser)]
#[command(
    name = "keylight",
    version,
    about = "Keylight management CLI — configure apps, licenses, and integrations from the terminal"
)]
struct Cli {
    /// Print machine-readable JSON instead of human-formatted output.
    #[arg(long, global = true)]
    json: bool,

    /// Never open a browser automatically; print URLs instead (also implied
    /// when stdout isn't a terminal).
    #[arg(long, global = true)]
    no_browser: bool,

    /// Management API base URL. Override for local dev / staging.
    #[arg(
        long,
        global = true,
        env = "KEYLIGHT_API_URL",
        default_value = "https://api.keylight.dev"
    )]
    api_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authorize the CLI via your browser
    Login {
        /// Permission to request; repeat for several. Defaults to every
        /// non-destructive scope.
        #[arg(long = "scope")]
        scope: Option<Vec<String>>,
    },
    /// Remove the locally stored API token. Does not revoke it server-side.
    Logout,
    /// Manage the stored API token.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Manage customers.
    Customers {
        #[command(subcommand)]
        command: commands::customers::CustomersCommand,
    },
    /// Manage payment-provider integrations.
    Integrations {
        #[command(subcommand)]
        command: commands::integrations::IntegrationsCommand,
    },
    /// Manage licenses.
    Licenses {
        #[command(subcommand)]
        command: commands::licenses::LicensesCommand,
    },
    /// Manage apps and their license key types.
    Products {
        #[command(subcommand)]
        command: commands::products::ProductsCommand,
    },
    /// Read or rotate the SDK key.
    SdkKey {
        #[command(subcommand)]
        command: commands::misc::SdkKeyCommand,
    },
    /// Run and inspect test purchases.
    TestPurchase {
        #[command(subcommand)]
        command: commands::misc::TestPurchaseCommand,
    },
    /// Show activation and validation volume.
    Usage {
        /// How many days back to report.
        #[arg(long)]
        days: Option<u32>,
    },
    /// Configure the outbound webhook.
    Webhook {
        #[command(subcommand)]
        command: commands::misc::WebhookCommand,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Store an API token, read from stdin (never a CLI argument — argv is
    /// visible in shell history and to other users via `ps`).
    ///
    /// Interactive:  keylight auth set-token
    /// Scripted:     echo "$KEYLIGHT_TOKEN" | keylight auth set-token
    SetToken,
    /// Show which tenant/token/scopes the current auth resolves to.
    Status,
}

fn main() {
    let cli = Cli::parse();
    let config_path = match config::config_path() {
        Some(p) => p,
        None => {
            eprintln!("error: could not determine a config directory for this platform");
            std::process::exit(1);
        }
    };
    let ctx = Context {
        json: cli.json,
        no_browser: cli.no_browser,
        api_url: cli.api_url,
        config_path,
        is_tty: std::io::stdout().is_terminal(),
    };
    let deps = Deps::real();

    let result = match cli.command {
        Command::Login { scope } => commands::login::login(&ctx, &deps, scope),
        Command::Logout => commands::logout::logout(&ctx),
        Command::Auth { command } => match command {
            AuthCommand::SetToken => {
                let stdin = std::io::stdin();
                let mut lock = stdin.lock();
                commands::auth::set_token(&ctx, &mut lock)
            }
            AuthCommand::Status => commands::auth::status(&ctx, &deps),
        },
        Command::Customers { command } => commands::customers::run(&ctx, &deps, command),
        Command::Integrations { command } => commands::integrations::run(&ctx, &deps, command),
        Command::Licenses { command } => commands::licenses::run(&ctx, &deps, command),
        Command::Products { command } => commands::products::run(&ctx, &deps, command),
        Command::SdkKey { command } => commands::misc::run_sdk_key(&ctx, &deps, command),
        Command::TestPurchase { command } => {
            commands::misc::run_test_purchase(&ctx, &deps, command)
        }
        Command::Usage { days } => commands::misc::run_usage(&ctx, &deps, days),
        Command::Webhook { command } => commands::misc::run_webhook(&ctx, &deps, command),
    };

    if let Err(e) = result {
        if ctx.json {
            let body = serde_json::json!({ "error": e.to_string() });
            println!("{body}");
        } else {
            eprintln!("error: {e}");
        }
        std::process::exit(exit_code(&e));
    }
}

/// A stable, if coarse, exit-code contract for scripts: `2` for auth
/// problems (fixable with `auth set-token`/`login`), `3` for a rejected
/// confirmation, `1` for everything else.
fn exit_code(e: &CliError) -> i32 {
    match e {
        CliError::NoToken
        | CliError::Api { status: 401, .. }
        | CliError::Api { status: 403, .. } => 2,
        CliError::ConfirmationNotApproved { .. } | CliError::ConfirmationTimedOut => 3,
        _ => 1,
    }
}
