//! `ackinacki-bridge` entrypoint.
//!
//! This file owns three things and nothing else:
//! 1. Wire the module tree together.
//! 2. Set up tracing (stderr, `RUST_LOG` env filter, no ANSI when not a TTY).
//! 3. Dispatch subcommands to [`orchestrator::run`] and translate the typed
//!    [`errors::CliError`] into a process exit code per
//!    [`errors::ExitCode`]'s wire contract.
//!
//! All human vs. `--json` formatting is delegated to [`output`]. All
//! validation lives in [`args`]. There is deliberately no business logic
//! here — a bug in this file should be trivially visible as
//! misrouting/misformatting, never a pipeline mistake.

mod args;
mod burn;
mod errors;
mod idempotency;
mod orchestrator;
mod output;
mod preflight;
mod resurrect;

use std::io::IsTerminal;
use std::process::ExitCode as ProcExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::args::{Cli, Command};

fn main() -> ProcExitCode {
    // Auto-source $BRIDGE_CONFIG profile file into process env BEFORE
    // clap reads any env= attr. dotenvy::from_path does NOT overwrite
    // vars already set in the shell — so precedence is preserved:
    //     explicit --flag > shell env > profile file > compiled default.
    // BRIDGE_CONFIG unset = no-op (env-only invocations still work).
    if let Ok(path) = std::env::var("BRIDGE_CONFIG") {
        if let Err(e) = dotenvy::from_path(&path) {
            eprintln!("fatal: BRIDGE_CONFIG={path} could not be loaded: {e}");
            return ProcExitCode::from(1);
        }
    }
    let cli = Cli::parse();
    init_tracing();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fatal: failed to start tokio runtime: {e}");
            return ProcExitCode::from(1);
        }
    };

    let json = cli.json;
    let result = rt.block_on(dispatch(cli));

    match result {
        Ok(summary) => {
            output::print_success(&summary, json);
            ProcExitCode::from(errors::ExitCode::Success.as_i32() as u8)
        }
        Err(e) => {
            output::print_error(&e, json);
            ProcExitCode::from(e.exit_code().as_i32() as u8)
        }
    }
}

/// Route the top-level subcommand. Kept as a thin async fn so
/// exit-code mapping stays in `main` and the orchestrator stays free of
/// process concerns.
async fn dispatch(cli: Cli) -> errors::CliResult<orchestrator::WithdrawSuccess> {
    // `--non-interactive` + not `--yes` means "refuse if we would prompt".
    // The prompt itself is orchestrator-owned (it's the last thing before
    // spending money), but the policy check we can front-load here.
    let non_interactive_needs_prompt = cli.non_interactive && !cli.yes;
    let skip_prompt = cli.yes;

    match cli.cmd {
        Command::Withdraw(args) => {
            if non_interactive_needs_prompt && !args.dry_run {
                return Err(errors::CliError::Preflight {
                    reason: "--non-interactive requires --yes or --dry-run \
                             (would otherwise block on confirmation prompt)"
                        .to_string(),
                    source: None,
                });
            }
            let dry_run = args.dry_run;
            orchestrator::run(args, dry_run, skip_prompt).await
        }
    }
}

/// Tracing → stderr. Respects `RUST_LOG`; defaults to `info` for our crate
/// and `warn` for everything else so a chatty dependency doesn't drown out
/// the pipeline story. ANSI colors only when stderr is a TTY.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("warn,ackinacki_bridge=info,bridge_relayer_daemon=info")
    });
    let is_tty = std::io::stderr().is_terminal();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_tty)
        .with_target(false)
        .try_init();
}
