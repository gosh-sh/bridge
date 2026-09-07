//! `ackinacki-bridge` entrypoint.
//!
//! This file owns three things and nothing else:
//! 1. Wire the module tree together.
//! 2. Set up tracing (stderr, `RUST_LOG` env filter, no ANSI when not a TTY).
//! 3. Dispatch subcommands to [`orchestrator::run`] and translate the typed
//!    [`errors::CliError`] into a process exit code per [`errors::ExitCode`]'s
//!    wire contract.
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
#[cfg(test)]
mod test_keys;

use std::{io::IsTerminal, process::ExitCode as ProcExitCode};

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::args::{Cli, Command};

fn main() -> ProcExitCode {
    // `--json` is a clap flag, but the three failures below happen before
    // (or during) parsing, and the spec allows no human line on stdout for
    // a --json run. Scan argv literally, just to pick the sink; `cli.json`
    // takes over the moment there is a parsed `Cli`.
    let json = output::wants_json(std::env::args());

    // Auto-source $BRIDGE_CONFIG profile file into process env BEFORE
    // clap reads any env= attr. dotenvy::from_path does NOT overwrite
    // vars already set in the shell — so precedence is preserved:
    //     explicit --flag > shell env > profile file > compiled default.
    // BRIDGE_CONFIG unset = no-op (env-only invocations still work).
    if let Ok(path) = std::env::var("BRIDGE_CONFIG") {
        if let Err(e) = dotenvy::from_path(&path) {
            let err = errors::CliError::Usage {
                reason: format!("BRIDGE_CONFIG={path} could not be loaded: {e}"),
            };
            output::print_error(&err, json);
            return ProcExitCode::from(err.exit_code().as_i32() as u8);
        }
    }

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // clap's own rendering is the best human text there is; under
            // --json it becomes the `message` field instead of going to
            // stderr raw. `--help` / `--version` are not errors: let clap
            // print them and exit 0.
            //
            // ONLY the two kinds a user gets by asking. Deliberately not
            // DisplayHelpOnMissingArgumentOrSubcommand: that is the
            // missing-subcommand family, i.e. a usage error, and putting it
            // here would hand `--json` a help page and exit 0 for what is a
            // refusal. (This build emits MissingSubcommand today; the other
            // kind appears the moment anyone sets `arg_required_else_help`,
            // so exclude it now rather than discover it later.)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = e.print();
                return ProcExitCode::from(0);
            }
            // Strip clap's own `error: ` prefix. `print_error`'s human
            // branch adds one, and clap's rendering already carries it, so
            // keeping both prints `error: error: the following required
            // arguments…`. The `--json` branch never had the problem —
            // it puts `reason` straight into the `message` field — which
            // is exactly why it is worth stripping here rather than
            // dropping the prefix in `print_error`, where every other
            // variant depends on it.
            let rendered = e.render().to_string();
            let rendered = rendered.trim_end();
            let reason = rendered
                .strip_prefix("error: ")
                .unwrap_or(rendered)
                .to_string();
            let err = errors::CliError::Usage {
                reason,
            };
            output::print_error(&err, json);
            return ProcExitCode::from(err.exit_code().as_i32() as u8);
        },
    };
    init_tracing();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Nothing has run, so this is a pre-send refusal like any
            // other. Rare (thread/fd exhaustion), and it is the one failure
            // that shows up exactly when a machine is already unhealthy and
            // a script most needs a parseable answer.
            let err = errors::CliError::Usage {
                reason: format!("failed to start the async runtime: {e}"),
            };
            output::print_error(&err, json);
            return ProcExitCode::from(err.exit_code().as_i32() as u8);
        },
    };

    // The parsed value governs everything downstream; the argv scan above
    // existed only for the three pre-parse escapes.
    let json = cli.json;
    let result = rt.block_on(dispatch(cli));

    match result {
        Ok(summary) => {
            output::print_success(&summary, json);
            ProcExitCode::from(errors::ExitCode::Success.as_i32() as u8)
        },
        Err(e) => {
            output::print_error(&e, json);
            ProcExitCode::from(e.exit_code().as_i32() as u8)
        },
    }
}

/// Route the top-level subcommand. Kept as a thin async fn so
/// exit-code mapping stays in `main` and the orchestrator stays free of
/// process concerns.
async fn dispatch(cli: Cli) -> errors::CliResult<orchestrator::WithdrawSuccess> {
    // --non-interactive alone means "refuse rather than block on a prompt".
    // Together with --yes there is no prompt to block on, so the run
    // proceeds. That pairing is the normal shape for a CI wrapper.
    //
    // The prompt itself is orchestrator-owned (it's the last thing before
    // spending money), but the policy check we can front-load here.
    let non_interactive_needs_prompt = cli.non_interactive && !cli.yes;
    let skip_prompt = cli.yes;

    match cli.cmd {
        Command::Withdraw(args) => {
            if non_interactive_needs_prompt && !args.dry_run {
                return Err(errors::CliError::Preflight {
                    reason: "--non-interactive requires --yes or --dry-run (would otherwise block \
                             on confirmation prompt)"
                        .to_string(),
                    source: None,
                });
            }
            let dry_run = args.dry_run;
            orchestrator::run(args, dry_run, skip_prompt).await
        },
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
