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

use clap::{CommandFactory, Parser, Subcommand};
use keylight_cli::commands::{self, Context, Deps};
use keylight_cli::config;
use std::io::IsTerminal;
use std::path::PathBuf;

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
    /// Set Keylight up in the current project — pick SDK or API and wire it up.
    Init {
        /// Force the integration mode instead of auto-detecting the project.
        #[arg(long, value_parser = ["sdk", "api"])]
        mode: Option<String>,
        /// App to wire up (API mode). Optional when the tenant has exactly one.
        #[arg(long)]
        product: Option<String>,
        /// Project directory to inspect and write into. Defaults to the CWD.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Env file to write the runtime token into (API mode).
        #[arg(long, default_value = ".env")]
        env_file: String,
    },
    /// Authorize the CLI via your browser
    Login {
        /// Permission to request; repeat for several. Defaults to every
        /// non-destructive scope.
        #[arg(long = "scope")]
        scope: Option<Vec<String>>,
    },
    /// Revoke the API token server-side and remove the local copy.
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
    /// One-time setup checks.
    Setup {
        #[command(subcommand)]
        command: commands::setup::SetupCommand,
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
    /// Print a shell completion script to stdout.
    ///
    /// Hidden because it is packaging plumbing, not a day-to-day command: the
    /// Homebrew formula and any manual install run it once to drop the script
    /// where the shell looks for it.
    ///
    ///     keylight completions zsh > ~/.zsh/completions/_keylight
    #[command(hide = true)]
    Completions {
        /// Shell to generate the script for.
        #[arg(value_name = "SHELL")]
        shell: clap_complete::Shell,
    },
    /// Render the man page (roff for `keylight.1`) to stdout.
    ///
    /// Hidden for the same reason as `completions` — the release tarball and
    /// the Homebrew formula generate `keylight.1` from this; humans read
    /// `--help`.
    #[command(hide = true)]
    Man,
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
    // clap exits `2` for a parser error, but its message is unstructured and
    // cannot distinguish a typo from this command's documented input errors.
    // Keep parser failures at `1`; command-level usage errors use CliError's
    // stable `2` instead, alongside authentication problems that need action.
    //
    // `--help` and `--version` also arrive here as errors and must still exit
    // `0` on stdout — `use_stderr()` is what separates the two.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            std::process::exit(if e.use_stderr() { 1 } else { 0 });
        }
    };
    // Completion scripts and the man page are rendered from the parsed command
    // tree itself, so they need neither a config directory nor the network.
    // Handle them before either is resolved.
    match &cli.command {
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let bin = cmd.get_name().to_string();
            clap_complete::generate(*shell, &mut cmd, bin, &mut std::io::stdout());
            return;
        }
        Command::Man => {
            clap_mangen::Man::new(Cli::command())
                .render(&mut std::io::stdout())
                .expect("rendering the man page to stdout cannot fail");
            return;
        }
        _ => {}
    }

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
        Command::Init {
            mode,
            product,
            dir,
            env_file,
        } => commands::init::init(&ctx, &deps, mode, product, dir, env_file),
        Command::Login { scope } => commands::login::login(&ctx, &deps, scope),
        Command::Logout => commands::logout::logout(&ctx, &deps),
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
        Command::Setup { command } => commands::setup::run(&ctx, &deps, command),
        Command::SdkKey { command } => commands::misc::run_sdk_key(&ctx, &deps, command),
        Command::TestPurchase { command } => {
            commands::misc::run_test_purchase(&ctx, &deps, command)
        }
        Command::Usage { days } => commands::misc::run_usage(&ctx, &deps, days),
        Command::Webhook { command } => commands::misc::run_webhook(&ctx, &deps, command),
        // Rendered and returned above, before any config/network work.
        Command::Completions { .. } | Command::Man => {
            unreachable!("completions/man are handled before dispatch")
        }
    };

    if let Err(e) = result {
        if ctx.json {
            let body = serde_json::json!({ "error": e.to_string() });
            println!("{body}");
        } else {
            eprintln!("error: {e}");
        }
        std::process::exit(e.exit_code());
    }
}
