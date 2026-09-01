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

use serde_json::json;

use crate::errors::CliError;
use crate::orchestrator::WithdrawSuccess;

/// Print a successful terminal summary. Chooses stderr-human or
/// stdout-json based on `json`.
pub fn print_success(summary: &WithdrawSuccess, json_mode: bool) {
    if json_mode {
        // One line to stdout. `WithdrawSuccess` is `Serialize` and holds
        // no key material.
        match serde_json::to_string(summary) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("output: failed to serialize success as JSON: {e}"),
        }
        return;
    }

    // Human summary — final block of stderr, distinct from tracing lines.
    let s = &summary.submit;
    let status = match s.status {
        crate::orchestrator::SubmitStatus::DryRunOk => "dry-run-ok",
        crate::orchestrator::SubmitStatus::Confirmed => "confirmed",
    };
    eprintln!();
    eprintln!("withdraw complete:");
    eprintln!("  amount:       {} USDC", summary.burn.amount);
    eprintln!("  AN tx:        {}", summary.burn.an_tx);
    eprintln!("  msg id:       {}", summary.capture.withdrawal_msg_id);
    eprintln!(
        "  block:        seq={} id={}",
        summary.capture.block_seq_no, summary.capture.block_id
    );
    eprintln!(
        "  proof:        {} bytes, {} public inputs, self_verified={}",
        summary.proof.calldata_bytes, summary.proof.pi_count, summary.proof.self_verified
    );
    match s.eth_tx.as_deref() {
        Some(tx) => eprintln!("  ETH tx:       {tx} ({status})"),
        None => eprintln!("  ETH tx:       (none) ({status})"),
    }
}

/// Print a terminal error. For `json`, emits `{"error":{...}}` on stdout
/// (per the machine-contract discipline). For human, prints to stderr.
/// **Never** includes key material, key file contents, or the ETH
/// private key. `CliError`'s Display impls are already scrubbed — this
/// fn just picks the sink and the shape.
pub fn print_error(err: &CliError, json_mode: bool) {
    if json_mode {
        let value = json!({
            "error": {
                "stage": err.stage(),
                "exit_code": err.exit_code().as_i32(),
                "message": format!("{err}"),
            }
        });
        // Fall back to a raw string if serde ever fails (it won't for the
        // shape above; belt-and-suspenders).
        match serde_json::to_string(&value) {
            Ok(s) => println!("{s}"),
            Err(_) => println!(
                r#"{{"error":{{"stage":"unknown","exit_code":{},"message":"serialization failed"}}}}"#,
                err.exit_code().as_i32()
            ),
        }
        return;
    }
    eprintln!("error: {err}");
    // Walk the `#[source]` chain so the underlying anyhow context (e.g.
    // aggregator stderr, RelayerError::other messages) is visible without
    // needing a debug build. `CliError` variants carry
    // `#[source] Option<anyhow::Error>`, and the anyhow chain itself may
    // nest further. Each hop indented for readability.
    let mut cause: Option<&(dyn std::error::Error + 'static)> =
        std::error::Error::source(err);
    let mut depth = 0usize;
    while let Some(c) = cause {
        eprintln!("  caused by [{depth}]: {c}");
        cause = c.source();
        depth += 1;
    }
}
