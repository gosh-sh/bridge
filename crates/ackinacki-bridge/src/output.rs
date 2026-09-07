//! Terminal output for success + error.
//!
//! Two output modes:
//! - **Human** — pretty stderr lines (tracing already handles logs; this module
//!   owns the FINAL summary the user sees after everything is done). Never
//!   printed to stdout.
//! - **`--json`** — one line JSON on stdout, nothing else on stdout, ever.
//!   Stdout is a machine contract; stderr is the human tap.
//!
//! Both paths route through the same [`WithdrawSuccess`] /
//! [`crate::errors::CliError`] types — no ad-hoc formatting in call
//! sites.

use serde_json::json;

use crate::{errors::CliError, orchestrator::WithdrawSuccess};

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

/// Literal scan of argv for `--json`.
///
/// Needed because the earliest failures — an unloadable `$BRIDGE_CONFIG`, a
/// clap parse error, a runtime that will not start — happen before there is
/// a parsed `Cli` to read `.json` from, and the spec says a `--json` run
/// emits a machine refusal in every case. Used ONLY to choose the sink for
/// those; everything after parsing uses `cli.json`.
///
/// Intentionally exact-match: no `--json=…`, no abbreviations, no
/// after-`--` handling. Getting this subtly different from clap would be
/// worse than the simple rule, and a wrong guess here only changes which
/// stream an error lands on.
pub fn wants_json(argv: impl Iterator<Item = String>) -> bool {
    argv.skip(1).any(|a| a == "--json")
}

/// The one-line `{"error":{…}}` envelope. Single source of truth for the
/// machine refusal shape, lifted out of [`print_error`] so it can be
/// asserted on directly.
pub fn error_json(err: &CliError) -> String {
    let value = json!({
        "error": {
            "stage": err.stage(),
            "exit_code": err.exit_code().as_i32(),
            "message": format!("{err}"),
        }
    });
    // Fall back to a raw string if serde ever fails (it won't for the shape
    // above; belt-and-suspenders).
    serde_json::to_string(&value).unwrap_or_else(|_| {
        format!(
            r#"{{"error":{{"stage":"unknown","exit_code":{},"message":"serialization failed"}}}}"#,
            err.exit_code().as_i32()
        )
    })
}

/// Print a terminal error. For `json`, emits `{"error":{...}}` on stdout
/// (per the machine-contract discipline). For human, prints to stderr.
/// **Never** includes key material, key file contents, or the ETH
/// private key. `CliError`'s Display impls are already scrubbed — this
/// fn just picks the sink and the shape.
pub fn print_error(err: &CliError, json_mode: bool) {
    if json_mode {
        println!("{}", error_json(err));
        return;
    }
    eprintln!("error: {err}");
    // Walk the `#[source]` chain so the underlying anyhow context (e.g.
    // aggregator stderr, RelayerError::other messages) is visible without
    // needing a debug build. `CliError` variants carry
    // `#[source] Option<anyhow::Error>`, and the anyhow chain itself may
    // nest further. Each hop indented for readability.
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    let mut depth = 0usize;
    while let Some(c) = cause {
        eprintln!("  caused by [{depth}]: {c}");
        cause = c.source();
        depth += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `&[&str]` carries two lifetimes, so a bare `'_` on the return type
    // is ambiguous (E0106). Name the outer one: the iterator borrows the
    // slice and yields owned Strings.
    fn argv<'a>(items: &'a [&str]) -> impl Iterator<Item = String> + 'a {
        items.iter().map(|s| s.to_string())
    }

    #[test]
    fn wants_json_finds_the_flag_anywhere() {
        assert!(wants_json(argv(&[
            "ackinacki-bridge",
            "--json",
            "withdraw"
        ])));
        assert!(wants_json(argv(&[
            "ackinacki-bridge",
            "withdraw",
            "--json"
        ])));
        assert!(wants_json(argv(&[
            "ackinacki-bridge",
            "withdraw",
            "--amount",
            "1",
            "--json"
        ])));
    }

    #[test]
    fn wants_json_does_not_guess() {
        assert!(!wants_json(argv(&["ackinacki-bridge", "withdraw"])));
        assert!(!wants_json(argv(&["ackinacki-bridge", "--jso"])));
        assert!(!wants_json(argv(&["ackinacki-bridge", "--json-lines"])));
        // A literal `--json` after `--` is an operand, not our flag; we
        // accept this false positive knowingly (the CLI takes no positional
        // operands) rather than reimplementing clap's parser.
    }

    #[test]
    fn usage_error_serialises_as_the_machine_contract() {
        let err = CliError::Usage {
            reason: "the following required arguments were not provided: --amount".into(),
        };
        assert_eq!(err.exit_code().as_i32(), 2);

        let json = error_json(&err);
        let v: serde_json::Value = serde_json::from_str(&json).expect("must be one JSON object");
        assert_eq!(v["error"]["stage"], "preflight");
        assert_eq!(v["error"]["exit_code"], 2);
        assert!(
            v["error"]["message"].as_str().unwrap().contains("--amount"),
            "must carry clap's text so a human can still read it: {json}",
        );
        assert!(!json.contains('\n'), "must be a single line: {json}");
    }
}
