//! Terminal output for success + error.
//!
//! Two output modes:
//! - **Human** — pretty stderr lines (tracing already handles logs; this
//!   module owns the FINAL summary the user sees after everything is
//!   done). Never printed to stdout.
//! - **`--json`** — one line JSON on stdout, nothing else on stdout, ever.
//!   Stdout is a machine contract; stderr is the human tap.
//!
//! Both paths route through the same [`WithdrawSuccess`] /
//! [`crate::errors::CliError`] types — no ad-hoc formatting in call
//! sites.

use crate::errors::CliError;
use crate::orchestrator::WithdrawSuccess;

/// Print a successful terminal summary. Chooses stderr-human or
/// stdout-json based on `json`.
pub fn print_success(_summary: &WithdrawSuccess, _json: bool) {
    unimplemented!("output::print_success — implement in third commit")
}

/// Print a terminal error. For `json`, emits `{"error":{...}}` on stdout
/// (per the machine-contract discipline). For human, prints to stderr.
/// **Never** includes key material, key file contents, or the ETH
/// private key.
pub fn print_error(_err: &CliError, _json: bool) {
    unimplemented!("output::print_error — implement in third commit")
}
