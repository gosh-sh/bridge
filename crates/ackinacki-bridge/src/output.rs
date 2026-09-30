//! Terminal output for success + error.
//!
//! Two output modes:
//! - **Human** — pretty stderr lines (tracing already handles logs; this module
//!   owns the FINAL summary the user sees after everything is done). The
//!   withdrawal summary is never printed to stdout; the deposit summary is,
//!   beside the deposit's QR codes, so a script wrapping a human run can
//!   capture it.
//! - **`--json`** — one line JSON on stdout, nothing else on stdout, ever.
//!   Stdout is a machine contract; stderr is the human tap.
//!
//! Both paths route through the same [`WithdrawSuccess`] /
//! [`crate::errors::CliError`] types — no ad-hoc formatting in call
//! sites.

use serde_json::json;

use crate::{errors::CliError, orchestrator::WithdrawSuccess};

/// What a successful run of either subcommand returns to `main`.
#[derive(Debug)]
pub enum RunSuccess {
    /// A finished (or dry-run) withdrawal.
    Withdraw(WithdrawSuccess),
    /// A finished, dry-run or released deposit; boxed, as it is several
    /// times the size of a withdrawal's summary.
    Deposit(Box<crate::deposit::DepositSuccess>),
}

/// Write a whole block to stdout without panicking on failure.
///
/// `println!` panics when the write fails, which turns a full disk, a
/// closed pipe or a `> /dev/full` into **exit 101** — discarding the
/// exit-code contract this module exists to serve. Reproduced:
/// `ackinacki-bridge --json withdraw > /dev/full` exited 101, and so did
/// `withdraw 2>/dev/full`.
///
/// The worst case is [`print_success`], which is reached only after the
/// burn landed AND `withdrawByProof` was mined. Value has moved on both
/// chains and the wrapper is told the process died of something
/// unknown — the one reading a script is most likely to retry.
///
/// The exit code is the answer, not the printing. So: report whether the
/// write landed, and let the caller decide; never take the process down
/// for it.
fn write_stdout(s: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(s.as_bytes())?;
    out.flush()
}

/// As [`write_stdout`], for stderr.
fn write_stderr(s: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    err.write_all(s.as_bytes())?;
    err.flush()
}

/// One of the two terminal streams, as something selectable at runtime.
type Sink = fn(&str) -> std::io::Result<()>;

/// Put `s` in front of the operator, preferring `primary`.
///
/// One fallback hop, not a loop: a `--json` consumer whose stdout is full
/// may still have a working stderr, and a human whose stderr is
/// redirected into a full file may still have a terminal on stdout. If
/// both are gone there is nothing left to say and nothing to be gained by
/// dying over it — the exit code still carries the outcome, which is the
/// part a script reads.
///
/// The fallback copy is prefixed, so nobody mistakes a rescued line for
/// output that arrived on the stream it was addressed to. For `--json`
/// that matters: an envelope on stderr is not the machine contract, and
/// must not be parsed as though it were.
fn emit(s: &str, to_stdout: bool) {
    let (primary, secondary): (Sink, Sink) = if to_stdout {
        (write_stdout, write_stderr)
    } else {
        (write_stderr, write_stdout)
    };
    if primary(s).is_ok() {
        return;
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the fallback stream is the last one there is; if it fails too there is nowhere \
                  left to report it"
    )]
    let _ = secondary(&format!(
        "ackinacki-bridge: could not write the line below to its own stream; it is repeated here \
         and is NOT the machine output\n{s}"
    ));
}

/// Print a successful terminal summary. Chooses stderr-human or
/// stdout-json based on `json`.
pub fn print_success(summary: &RunSuccess, json_mode: bool) {
    match summary {
        RunSuccess::Withdraw(w) => print_withdraw(w, json_mode),
        RunSuccess::Deposit(d) => print_deposit(d, json_mode),
    }
}

/// The withdrawal summary, in the sink `json_mode` selects.
fn print_withdraw(summary: &WithdrawSuccess, json_mode: bool) {
    if json_mode {
        // One line to stdout. `WithdrawSuccess` is `Serialize` and holds
        // no key material.
        match serde_json::to_string(summary) {
            Ok(s) => emit(&format!("{s}\n"), true),
            Err(e) => emit(
                &format!("output: failed to serialize success as JSON: {e}\n"),
                false,
            ),
        }
        return;
    }

    // Human summary — final block of stderr, distinct from tracing lines.
    let s = &summary.submit;
    let status = match s.status {
        crate::orchestrator::SubmitStatus::DryRunOk => "dry-run-ok",
        crate::orchestrator::SubmitStatus::Confirmed => "confirmed",
    };
    // Built whole, then written once. Sixteen separate `eprintln!`s were
    // sixteen chances to panic and sixteen chances to tear the summary in
    // half — the same reasoning that replaced the burn confirmation
    // prompt's discarded writes with one checked `write_all`.
    let eth = match s.eth_tx.as_deref() {
        Some(tx) => format!("  ETH tx:       {tx} ({status})\n"),
        None => format!("  ETH tx:       (none) ({status})\n"),
    };
    emit(
        &format!(
            "\nwithdraw complete:\n\x20 amount:       {} USDC\n\x20 AN tx:        {}\n\x20 msg \
             id:       {}\n\x20 block:        seq={} id={}\n\x20 proof:        {} bytes, {} \
             public inputs, self_verified={}\n{eth}",
            summary.burn.amount,
            summary.burn.an_tx,
            summary.capture.withdrawal_msg_id,
            summary.capture.block_seq_no,
            summary.capture.block_id,
            summary.proof.calldata_bytes,
            summary.proof.pi_count,
            summary.proof.self_verified,
        ),
        false,
    );
}

/// The deposit summary, in the sink `json_mode` selects.
fn print_deposit(s: &crate::deposit::DepositSuccess, json_mode: bool) {
    if json_mode {
        match serde_json::to_string(s) {
            Ok(line) => emit(&format!("{line}\n"), true),
            Err(e) => emit(
                &format!("output: failed to serialize success as JSON: {e}\n"),
                false,
            ),
        }
        return;
    }
    emit(&deposit_summary(s), DEPOSIT_SUMMARY_TO_STDOUT);
}

/// Where the human deposit summary goes. Stdout, so a script wrapping a
/// human run can still capture the one thing it prints; logs and the
/// checklist stay on stderr.
const DEPOSIT_SUMMARY_TO_STDOUT: bool = true;

/// The human deposit summary as one block of text.
fn deposit_summary(s: &crate::deposit::DepositSuccess) -> String {
    let field = |v: &Option<serde_json::Value>, k: &str| {
        v.as_ref()
            .and_then(|x| x.get(k))
            .filter(|x| !x.is_null())
            .map(|x| {
                x.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| x.to_string())
            })
            .unwrap_or_else(|| "-".into())
    };
    let head = if s.dry_run {
        "deposit dry run: nothing was sent"
    } else if s.abandoned {
        "deposit operation released"
    } else {
        "deposit complete"
    };
    let mut out = format!(
        "\n{head}:\n\x20 network:      {} ({})\n\x20 amount:       {} USDC\n\x20 to:           \
         {}\n",
        s.network, s.chain_id, s.amount, s.to,
    );
    if s.dry_run {
        out.push_str(&dry_run_lines(s));
        return out;
    }
    out.push_str(&format!(
        "\x20 operation:    {}\n",
        s.op_id.as_deref().unwrap_or("-")
    ));
    let deposit = format!(
        "\x20 deposit:      tx {} depositId {}\n",
        field(&s.deposit, "tx_hash"),
        field(&s.deposit, "deposit_id"),
    );
    if s.abandoned {
        // Released before it finished: only what is known.
        if s.deposit.is_some() {
            out.push_str(&deposit);
        }
        return out;
    }
    // The anchor line is left out when the writer is unknown, never
    // printed as "null".
    let anchor = match s
        .anchor
        .as_ref()
        .and_then(|a| a.get("writer"))
        .and_then(|w| w.as_str())
    {
        Some("owner") => "  anchored by:  bridge owner\n".to_string(),
        Some("light-client") => "  anchored by:  light client\n".to_string(),
        _ => String::new(),
    };
    out.push_str(&format!(
        "{deposit}{anchor}\x20 credited:     confirmDeposit {} delivery {}\n\x20 balance:      {} \
         -> {} (diagnostic only)\n",
        field(&s.confirmation, "confirm_tx"),
        field(&s.confirmation, "delivery_tx"),
        field(&s.balance, "before"),
        field(&s.balance, "after"),
    ));
    out
}

/// What a dry run adds to the deposit summary: whom the anchor wait would
/// wait for, and both transactions as calldata and as QR payloads. A value
/// the summary does not carry is left out, not printed as a dash.
fn dry_run_lines(s: &crate::deposit::DepositSuccess) -> String {
    let tx = |k: &str| {
        s.tx.as_ref()
            .and_then(|t| t.get(k))
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    let codes: Vec<String> =
        s.tx.as_ref()
            .and_then(|t| t.get("eip681"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|u| u.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
    let mut out = String::new();
    if let Some(d) = anchor_decision(s) {
        out.push_str(&format!("\x20 anchor:       {d}\n"));
    }
    for (i, (name, calldata)) in [
        ("approve", "approve_calldata"),
        ("deposit", "deposit_calldata"),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(c) = tx(calldata) {
            out.push_str(&format!("\x20 {name}:      calldata {c}\n"));
        }
        if let Some(u) = codes.get(i) {
            out.push_str(&format!("\x20               EIP-681 QR {u}\n"));
        }
    }
    if let Some(w) = tx("walletconnect") {
        out.push_str(&format!("\x20 WalletConnect QR: {w}\n"));
    }
    out
}

/// Whom a deposit's anchor wait would wait for, from the dry run's anchor
/// plan: the light client alone when the owner cannot anchor any more,
/// otherwise the bridge owner, and the light client too when it is ready.
/// `None` when the summary does not say.
fn anchor_decision(s: &crate::deposit::DepositSuccess) -> Option<&'static str> {
    let a = s.anchor.as_ref()?;
    let ready = a.get("light_client_ready").and_then(|v| v.as_bool()) == Some(true);
    match a.get("writer")?.as_str()? {
        "light-client" => Some("waiting for the light client"),
        "owner" if ready => Some("waiting for the bridge owner or the light client"),
        "owner" => Some("waiting for the bridge owner"),
        _ => None,
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

/// Walk a `#[source]` chain into a flat list, outermost cause first.
///
/// Shared by both output modes so they cannot disagree about what the
/// cause of a failure was — the human branch used to walk this and the
/// JSON branch did not, which is the whole bug below.
///
/// Bounded. An error graph is a chain by construction, but a
/// pathological `source()` that returns itself would spin here forever
/// while holding stdout, on a path that only runs when something has
/// already gone wrong. Twelve is far past anything this crate builds:
/// the deepest real chain is a `CliError` over an anyhow with a couple of
/// `.context()` hops over an io::Error.
fn cause_chain(err: &CliError) -> Vec<String> {
    let mut out = Vec::new();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(c) = cause {
        out.push(c.to_string());
        if out.len() == 12 {
            out.push("(cause chain truncated)".to_string());
            break;
        }
        cause = c.source();
    }
    out
}

/// The one-line `{"error":{…}}` envelope. Single source of truth for the
/// machine refusal shape, lifted out of [`print_error`] so it can be
/// asserted on directly.
///
/// **`causes` carries the `#[source]` chain, which this used to drop.**
/// `format!("{err}")` renders only the outermost `Display`, so everything
/// a stage wrapped — the aggregator's stderr, a `RelayerError`, the
/// keygen-lock timeout that names the process to wait for — was visible
/// in human mode and invisible under `--json`. A `--json` consumer got
/// "withdraw-e2e pipeline failed" and nothing about why, on the one
/// output mode a script is reading.
///
/// Additive, not a change to `message`: consumers already read that
/// field, and rewriting it to include the chain would silently change
/// what their pattern matches. `causes` is `[]` for the many errors that
/// carry no source.
///
/// In a deposit run the message and every cause are redacted: an RPC
/// provider's key sits in the path or query of its URL, and an HTTP
/// client's error — or the prover's stderr — quotes that URL whole.
pub fn error_json(err: &CliError) -> String {
    let hide = |s: &str| crate::deposit::ui::redact_in_deposit_run(s).into_owned();
    let causes: Vec<String> = cause_chain(err).iter().map(|c| hide(c)).collect();
    let mut value = json!({
        "error": {
            "stage": err.stage(),
            "exit_code": err.exit_code().as_i32(),
            "message": hide(&format!("{err}")),
            "causes": causes,
        }
    });
    if let Some(op) = err.op_id() {
        value["error"]["op_id"] = json!(op);
    }
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
        emit(&format!("{}\n", error_json(err)), true);
        return;
    }
    emit(&human_error(err), false);
}

/// The human error block: the error, then its causes, outermost first,
/// redacted in a deposit run as the JSON envelope is.
fn human_error(err: &CliError) -> String {
    let hide = |s: &str| crate::deposit::ui::redact_in_deposit_run(s).into_owned();
    let mut block = format!("error: {}\n", hide(&format!("{err}")));
    // The same walk the JSON envelope does, through the same function, so
    // the two modes cannot report different causes for one failure. The
    // underlying anyhow context (aggregator stderr, `RelayerError::other`
    // messages, the keygen-lock timeout) is what makes a stage-5 failure
    // diagnosable without a debug build.
    for (depth, c) in cause_chain(err).iter().enumerate() {
        block.push_str(&format!("  caused by [{depth}]: {}\n", hide(c)));
    }
    block
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
    fn the_deposit_envelope_names_the_operation() {
        let e = CliError::deposit(
            crate::errors::ExitCode::DepositOutcomeUnknown,
            crate::errors::Stage::Deposit,
            Some("01J9ZQ4X7T0000000000000000"),
            "the wallet request went out and no transaction was found",
        );
        let v: serde_json::Value = serde_json::from_str(&error_json(&e)).unwrap();
        assert_eq!(v["error"]["exit_code"], 30);
        assert_eq!(v["error"]["stage"], "deposit");
        assert_eq!(v["error"]["op_id"], "01J9ZQ4X7T0000000000000000");
    }

    #[test]
    fn a_withdrawal_envelope_has_no_op_id_key() {
        let e = CliError::Usage {
            reason: "x".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&error_json(&e)).unwrap();
        assert!(v["error"].get("op_id").is_none());
    }

    fn sample() -> crate::deposit::DepositSuccess {
        crate::deposit::DepositSuccess {
            op_id: Some("op1".into()),
            dry_run: false,
            network: "sepolia".into(),
            chain_id: 11155111,
            amount: "5".into(),
            to: "0:ab".into(),
            deposit: Some(json!({"tx_hash": "0xt", "deposit_id": 7})),
            anchor: Some(json!({"writer": "owner"})),
            tx: None,
            confirmation: Some(json!({"confirm_tx": "c", "delivery_tx": null})),
            balance: None,
            abandoned: false,
        }
    }

    #[test]
    fn the_deposit_summary_goes_to_stdout() {
        const { assert!(DEPOSIT_SUMMARY_TO_STDOUT) };
    }

    #[test]
    fn the_deposit_summary_names_who_anchored() {
        let mut d = sample();
        let text = deposit_summary(&d);
        assert!(text.contains("  anchored by:  bridge owner\n"), "{text}");
        assert!(text.contains("deposit complete:"), "{text}");
        assert!(text.contains("tx 0xt depositId 7"), "{text}");
        assert!(text.contains("delivery -\n"), "{text}");
        d.anchor = Some(json!({"writer": "light-client"}));
        assert!(deposit_summary(&d).contains("  anchored by:  light client\n"));
        d.anchor = Some(json!({"writer": null}));
        assert!(!deposit_summary(&d).contains("anchored by"));
        d.anchor = None;
        let text = deposit_summary(&d);
        assert!(
            !text.contains("anchored by") && !text.contains("null"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn the_dry_run_summary_prints_both_calldatas_the_codes_and_the_anchor_decision() {
        use crate::deposit::{
            args::RunMode,
            locks::DirLock,
            preflight::{check, dry_run_summary},
            store::Store,
            testkit::World,
        };
        for (owner, lc_ready, decision) in [
            (true, false, "waiting for the bridge owner\n"),
            (
                true,
                true,
                "waiting for the bridge owner or the light client\n",
            ),
            (false, true, "waiting for the light client\n"),
        ] {
            let world = World::healthy();
            world.owner_anchors(owner);
            if lc_ready {
                world.light_client_ready();
            }
            let p = world.params(RunMode::DryRun);
            let store = Store::open(&p.state_dir).unwrap();
            let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
            let c = check(&p, &world.deps(), &store, None).await.unwrap();
            let s = dry_run_summary(&p, &c);
            let text = deposit_summary(&s);
            let tx = s.tx.as_ref().unwrap();
            for k in ["approve_calldata", "deposit_calldata"] {
                let calldata = tx[k].as_str().unwrap();
                assert!(
                    calldata.len() > 10 && text.contains(calldata),
                    "{k}: {text}"
                );
            }
            let codes = tx["eip681"].as_array().unwrap();
            assert_eq!(codes.len(), 2);
            for u in codes {
                assert!(text.contains(u.as_str().unwrap()), "{text}");
            }
            assert!(text.contains("WalletConnect"), "{text}");
            assert!(
                text.contains(&format!("  anchor:       {decision}")),
                "owner={owner} lc={lc_ready}: {text}"
            );
            assert!(
                text.starts_with("\ndeposit dry run: nothing was sent:\n"),
                "{text}"
            );
            // Nothing was sent, so there is nothing to put a dash against.
            assert!(
                !text.contains(" -\n") && !text.contains("operation"),
                "{text}"
            );
        }
    }

    #[test]
    fn the_released_summary_prints_no_placeholder_lines() {
        let mut d = sample();
        d.abandoned = true;
        d.deposit = None;
        d.anchor = None;
        d.confirmation = None;
        let text = deposit_summary(&d);
        assert!(text.contains("deposit operation released:"), "{text}");
        assert!(text.contains("  operation:    op1\n"), "{text}");
        assert!(!text.contains(" -"), "{text}");
    }

    #[test]
    fn a_deposit_error_prints_the_rpc_url_without_its_key() {
        const URL: &str = "https://rpc.example.org/v3/InfuraKey-0c9b8a7d?token=TokenKey-e5f4d3c2";
        crate::deposit::ui::hide_url_secrets([URL]);
        // The prover's stderr tail: an HTTP client's error as `Debug` prints it.
        let tail = r#"Error: reqwest::Error { kind: Request, url: Url { scheme: "https", host: Some(Domain("rpc.example.org")), path: "/v3/InfuraKey-0c9b8a7d", query: Some("token=TokenKey-e5f4d3c2") } }"#;
        let e = CliError::deposit(
            crate::errors::ExitCode::DepositProofFailed,
            crate::errors::Stage::Prove,
            Some("01J9ZQ4X7T0000000000000000"),
            format!(
                "fetch_deposit_data failed (exit status: 1). The deposit is on the EVM bridge; \
                 retry with --resume 01J9ZQ4X7T0000000000000000\nlast lines of its stderr:\n{tail}"
            ),
        );
        let json = error_json(&e);
        let human = human_error(&e);
        for s in ["InfuraKey-0c9b8a7d", "TokenKey-e5f4d3c2"] {
            assert!(!json.contains(s), "{json}");
            assert!(!human.contains(s), "{human}");
        }
        assert!(
            json.contains("--resume 01J9ZQ4X7T0000000000000000"),
            "{json}"
        );
        assert!(!json.contains('\n'), "{json}");
        // An RPC error under a refusal: the cause names the provider only.
        let e = CliError::Preflight {
            reason: "could not read the chain id".into(),
            source: Some(anyhow::anyhow!(
                "error sending request for url (https://RPC.example.org/v3/InfuraKey-0c9b8a7d/)"
            )),
        };
        let v: serde_json::Value = serde_json::from_str(&error_json(&e)).unwrap();
        assert_eq!(
            v["error"]["causes"][0],
            "error sending request for url (https://RPC.example.org)"
        );
        let human = human_error(&e);
        assert!(!human.contains("InfuraKey"), "{human}");
        assert!(human.contains("https://RPC.example.org)"), "{human}");
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
        assert_eq!(
            v["error"]["causes"].as_array().map(Vec::len),
            Some(0),
            "an error with no source gets an empty list, not a missing field: {json}",
        );
    }

    #[test]
    fn the_json_envelope_carries_the_whole_cause_chain() {
        // `format!("{err}")` renders only the outermost Display, so
        // everything a stage wrapped was visible in human mode and
        // invisible under `--json` — on the one output mode a script
        // reads. A stage-5 failure said "withdraw-e2e pipeline failed"
        // and nothing about why: not the aggregator's stderr, not the
        // keygen-lock timeout naming the process to wait for.
        let inner = anyhow::anyhow!("flock held by pid 4242")
            .context("waited 30 minutes for the event keygen lock")
            .context("event keygen failed");
        let err = CliError::ProofFailed {
            reason: "withdraw-e2e pipeline failed: event keygen failed".into(),
            source: Some(inner),
        };

        let v: serde_json::Value = serde_json::from_str(&error_json(&err)).unwrap();
        let causes: Vec<&str> = v["error"]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(
            causes,
            vec![
                "event keygen failed",
                "waited 30 minutes for the event keygen lock",
                "flock held by pid 4242",
            ],
            "outermost first, every hop present",
        );
        assert!(
            !error_json(&err).contains('\n'),
            "still one line, however deep the chain",
        );
    }

    #[test]
    fn both_output_modes_report_the_same_causes() {
        // One walk, one function. The human branch had it and the JSON
        // branch did not, which is exactly how they came to disagree.
        let err = CliError::EthSubmitFailed {
            reason: "submit failed".into(),
            source: Some(anyhow::anyhow!("connection refused").context("eth_sendRawTransaction")),
        };
        let v: serde_json::Value = serde_json::from_str(&error_json(&err)).unwrap();
        let from_json: Vec<String> = v["error"]["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(from_json, cause_chain(&err));
        assert_eq!(from_json.len(), 2);
    }

    #[test]
    fn a_self_referential_cause_chain_terminates() {
        // Nothing in this crate builds one, and a real error graph is a
        // chain — but this walk runs while holding stdout on a path that
        // only executes when something has already failed, so it is
        // bounded rather than trusted.
        #[derive(Debug)]
        struct Loop;
        impl std::fmt::Display for Loop {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "loops back to itself")
            }
        }
        impl std::error::Error for Loop {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                // A fresh leak each hop, so the walk sees an endless
                // chain rather than a borrow of self.
                Some(Box::leak(Box::new(Loop)))
            }
        }
        let err = CliError::ProofFailed {
            reason: "x".into(),
            source: Some(anyhow::Error::new(Loop)),
        };
        let chain = cause_chain(&err);
        assert!(chain.len() <= 13, "bounded, got {}", chain.len());
        assert_eq!(chain.last().unwrap(), "(cause chain truncated)");
    }
}
