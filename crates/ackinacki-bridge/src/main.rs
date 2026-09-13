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

//! ---
//!
//! `let_underscore_must_use` is on for this crate. `#[must_use]` alone
//! does not cover `let _ = ..`, which is the form that silently drops a
//! `Result` — including a state-file write standing between a burn and
//! the record of it. Nine sites predate the lint and every one of them
//! is a deliberate discard; each now says so at the point of the
//! discard, and a tenth has to argue its case rather than blend in.
//!
//! `expect` rather than `allow`: it fires when the exemption stops being
//! needed, so a site that grows a real error path does not keep a stale
//! waiver.
#![warn(clippy::let_underscore_must_use)]

mod args;
mod burn;
mod errors;
mod idempotency;
mod orchestrator;
mod output;
mod preflight;
mod resurrect;
#[cfg(test)]
mod source_guard;
#[cfg(test)]
mod test_chain;
#[cfg(test)]
mod test_keys;

use std::{io::IsTerminal, process::ExitCode as ProcExitCode};

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::args::{Cli, Command};

/// argv in, exit code out: the three pre-parse escapes, clap, the
/// runtime, the dispatch policy, and the exit-code mapping.
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
    //
    // A `match`, not `if let Ok`. `std::env::var` has THREE answers and
    // the third one was being read as the first: a `BRIDGE_CONFIG` whose
    // bytes are not UTF-8 returns `NotUnicode`, and `if let Ok` skipped
    // the profile exactly as if the variable were unset — before
    // `init_tracing`, so without a line anywhere saying so.
    //
    // That is a path to a second `initiateWithdrawal`, not a
    // configuration annoyance. `BRIDGE_WITHDRAW_STATE_DIR` comes from the
    // profile; unsourced, `--state-dir` is `None` and the run falls back
    // to the default directory. The idempotency key is a hash of
    // `(from, to, chain, amount)` and does NOT include the directory, so
    // the reservation for this withdrawal, its record and its lock file
    // are all in the directory nobody is looking at: `peek` answers
    // `Ok(None)`, `reserve` answers `Created`, `decide_burn` answers
    // `Send`, and the multisig has no replay guard.
    match std::env::var("BRIDGE_CONFIG") {
        Ok(path) => {
            if let Err(e) = dotenvy::from_path(&path) {
                let err = errors::CliError::Usage {
                    // Redacted, like the branch below it: this path comes
                    // out of the environment of whoever ran the CLI, and a
                    // newline in it forges a line in the refusal it
                    // causes. The `NotUnicode` arm has scrubbed its value
                    // since the day it was written; this one interpolated
                    // raw, three lines away.
                    reason: format!(
                        "BRIDGE_CONFIG={} could not be loaded: {e}",
                        args::redact(&path)
                    ),
                };
                output::print_error(&err, json);
                return ProcExitCode::from(err.exit_code().as_i32() as u8);
            }
        },
        // Unset is a no-op: env-only invocations still work.
        Err(std::env::VarError::NotPresent) => {},
        Err(std::env::VarError::NotUnicode(raw)) => {
            // Refused rather than ignored, and exit 2 is the truth of it:
            // this happens before any chain is touched and before any
            // state directory is resolved, so nothing was broadcast and
            // nothing was written.
            //
            // The value is rendered lossily and then scrubbed. It came
            // from the environment of whoever ran this, it is by
            // definition malformed, and a bare newline or ANSI escape in
            // it would forge lines in the refusal it appears in.
            let err = errors::CliError::Usage {
                reason: format!(
                    "BRIDGE_CONFIG is set to a value that is not valid UTF-8 ({} bytes, shown \
                     lossily: {}), so the profile it names cannot be loaded.\n\x20 Refused rather \
                     than ignored: without the profile this run would fall back to the default \
                     withdrawal state directory, find no record of a withdrawal that has one, and \
                     broadcast a second burn for it.",
                    raw.len(),
                    args::redact(&raw.to_string_lossy()),
                ),
            };
            output::print_error(&err, json);
            return ProcExitCode::from(err.exit_code().as_i32() as u8);
        },
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
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "--help / --version with a closed stdout: nothing to say and nowhere \
                              to say it"
                )]
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
    // `try_init` fails only when a subscriber is already installed,
    // which in this binary means a test set one up on purpose.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a subscriber that is already installed is the caller's, not an error"
    )]
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_tty)
        .with_target(false)
        .try_init();
}
