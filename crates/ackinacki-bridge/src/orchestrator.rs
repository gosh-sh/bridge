//! Top-level orchestration. Composes the six pipeline stages behind a
//! single async entrypoint that `main` invokes.
//!
//! Stages, error-code-wise:
//! 1. **Preflight** ([`crate::preflight`]) — exit 2 on refusal.
//! 2. **Idempotency** ([`crate::idempotency`]) — exit 3 on duplicate.
//! 3. **Burn** ([`crate::burn`]) — exit 10 on unknown-outcome mid-send.
//! 4. **Capture** — chain-follows the multisig `an_tx_hash` through
//!    USDCBridge's `dst_transaction` to the WithdrawalInitiated ExtOut via
//!    [`bridge_relayer_daemon::withdraw_e2e::capture_targeted_withdrawal_event`].
//!    This is multi-user-safe: filtering by our specific tx hash instead of
//!    youngest-picking a shared USDCBridge queue means concurrent burns from
//!    other operators cannot be mis-selected as ours. Exit 11 on capture
//!    timeout (burn bounced, GQL unreachable, or USDCBridge never emitted the
//!    ExtOut).
//!
//! 4b. **Resurrect + coverage-wait** — see [`crate::resurrect`].
//!
//! 5. **Prove** — reuses
//!    [`bridge_relayer_daemon::withdraw_e2e::run_once_with_state`] with the
//!    just-captured event and the resurrected `BridgeState`. Exit 12 on prover
//!    failure.
//! 6. **Submit** — reuses [`bridge_relayer_daemon::bridge::EthBridgeClient`]
//!    `dry_run_withdraw` (always) and `submit_withdraw` (unless `--dry-run`).
//!    Exit 13 on revert.
//!
//! Every stage transition updates the idempotency record so a mid-flight
//! crash leaves a resumable trace. v1 doesn't implement `--resume`, but
//! the state file is written eagerly regardless so v2 has what it needs.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use alloy::{network::EthereumWallet, providers::ProviderBuilder, signers::Signer};
use bridge_event_witness::AnchorLayerMode;
use bridge_relayer_daemon::{
    bridge::{DryRunOutcome, EthBridgeClient, WithdrawSubmitOutcome},
    withdraw_e2e::{run_once_with_state, WithdrawE2EConfig},
};
use tracing::{info, warn};

use crate::{
    args::{self, FromAddress, ToAddress, UsdcAmount, WithdrawArgs},
    burn,
    errors::{CliError, CliResult},
    idempotency::{self, Status},
    preflight,
    resurrect::{covering_bundle_seq_no, stride_for, wait_for_coverage},
};

/// Default `WithdrawalInitiated` ExtOut `dst` sentinel — `makeAddrExtern(618)`.
/// Matches the relayer daemon CLI default (`bin/relayer.rs:470`).
const DEFAULT_EVENT_DST: &str = ":000000000000000000000000000000000000000000000000000000000000026a";

/// Capture wait budget — matches Python driver expectation (a few blocks
/// after the burn broadcast). Not exposed as a flag because v1 users don't
/// need to tune it; if they do we'll promote it later.
const EVENT_WAIT: Duration = Duration::from_secs(300);
const EVENT_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Coverage-wait budget. Sized for the worst case where a burn lands
/// just past a bundle boundary and the CLI must wait a full next-bundle
/// chain-time plus the parallel relayer's prove time. At L1
/// (`stride=1024`, shellnet 3 seq/s ≈ 5.7 min chain-time + prover
/// wall-time ~6-10 min → ~15 min); L2 (`stride=16384` → ~91 min
/// chain-time + prover). 2 h ceiling matches the daemon-side
/// `ENRICH_TIMEOUT`.
const COVERAGE_WAIT: Duration = Duration::from_secs(120 * 60);
const COVERAGE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Terminal success record — the one thing `main` prints (human or JSON).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WithdrawSuccess {
    pub burn: BurnSummary,
    pub capture: CaptureSummary,
    pub proof: ProofSummary,
    pub submit: SubmitSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BurnSummary {
    pub an_tx: String,
    pub bounce: bool,
    pub amount: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureSummary {
    pub withdrawal_msg_id: String,
    pub block_seq_no: u64,
    pub block_id: String,
    pub envelope_hash: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProofSummary {
    pub self_verified: bool,
    pub calldata_bytes: usize,
    pub pi_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SubmitSummary {
    /// `None` for `--dry-run` success; `Some(tx_hash)` for real submits.
    pub eth_tx: Option<String>,
    pub status: SubmitStatus,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitStatus {
    DryRunOk,
    Confirmed,
}

/// Full pipeline. `main` handles arg parsing, tracing setup, exit-code
/// mapping, and JSON vs. human output — this fn just runs the ballet.
///
/// `skip_prompt` — bypass the pre-burn confirmation prompt (`--yes`).
/// `--non-interactive` without `--yes` is refused upstream in `main` (by a
/// policy check, not by clap — the two flags combine freely), so
/// the orchestrator only needs the "may I skip the prompt?" bit here.
pub async fn run(
    args: WithdrawArgs,
    dry_run: bool,
    skip_prompt: bool,
) -> CliResult<WithdrawSuccess> {
    // ---- Arg validation (parses raw strings into typed forms) ----
    let from = args::parse_from(&args.from)?;
    let to = args::parse_to(&args.to, args.to_chain)?;
    let amount = args::parse_amount(&args.amount)?;
    let anchor_mode = parse_anchor_layer(&args.anchor_layer)?;

    // Resolve submit-only plumbing up front for a real run, so a missing
    // BURNER_PRIVATE_KEY refuses at stage 1 rather than at stage 6 — after
    // the burn is already irreversible.
    let plumbing = if dry_run {
        None
    } else {
        Some(args.require_submit_plumbing()?)
    };

    // Read the prior record BEFORE preflight. Read-only: `peek` never
    // creates anything, so this cannot poison a run that is about to be
    // refused. Used twice — here, to decide whether the ECC[3] sufficiency
    // check still applies, and in stage 2 to classify a duplicate before
    // prompting.
    // Resolved only when the run will actually use it.
    //
    // `default_state_dir` REFUSES when HOME is unset, which is right for a
    // real run — that directory is the only thing stopping a second burn.
    // A dry run neither reads nor writes it, so resolving it eagerly made
    // `--dry-run` fail under systemd, cron and most Docker images with a
    // double-burn refusal about a path it would never touch. The check
    // added to close a real hole started refusing the one command whose
    // whole purpose is to be safe to run anywhere.
    //
    // The dry-run placeholder is deliberately NOT `PathBuf::new()`:
    // joining a filename onto an empty path yields a relative one, which
    // is the cwd-relative state directory this refusal exists to prevent.
    // A path that cannot exist fails loudly instead — and nothing reaches
    // it, because `peek` is guarded below, the reservation lives inside
    // the `else` of `if dry_run`, and every `update` is behind
    // `record.as_mut()`, which stays `None` for a dry run.
    let state_dir = match (&args.state_dir, dry_run) {
        (Some(d), _) => d.clone(),
        (None, false) => default_state_dir()?,
        (None, true) => PathBuf::from("/nonexistent/dry-run-uses-no-state-dir"),
    };
    let prior = if dry_run {
        // A dry run neither reads nor writes idempotency state (spec).
        None
    } else {
        idempotency::peek(&state_dir, &from, &to, &amount)?
    };

    // A prior record carrying an an_tx_hash means the burn is already on
    // the wire. Its ECC[3] is spent by definition, and re-checking
    // sufficiency would refuse every resume of a full-balance withdrawal —
    // the exact scenario the record exists to rescue.
    let burn_already_sent = prior.as_ref().is_some_and(|p| p.an_tx_hash.is_some());
    // The same fact, carried as the hash itself, because a refusal below
    // has to NAME the transaction an operator must not act around.
    let recorded_an_tx: Option<&str> = prior.as_ref().and_then(|p| p.an_tx_hash.as_deref());

    // ---- 1. Preflight ----
    info!("stage 1/6: preflight");
    let preflight = preflight::run(
        &from,
        &args.from_keys,
        &to,
        &amount,
        &args.gql_endpoint,
        &args.usdc_bridge_account,
        preflight::BalanceCheck::from_burn_sent(burn_already_sent),
    )
    .await
    .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;
    info!(
        multisig_ecc3 = preflight.multisig_ecc3_balance,
        usdc_bridge = %preflight.usdc_bridge_extended,
        "preflight ok",
    );

    // Chain + deploy checks run ALWAYS, dry-run included: they need no
    // key, and "the RPC is not the chain you named" / "there is nothing at
    // that address" are exactly the irreversible mistakes a preflight
    // exists to catch.
    preflight::check_destination_chain(&args.rpc_url, to.chain_id)
        .await
        .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;
    info!(chain_id = to.chain_id, "destination chain ok");

    // The identity our proof will carry. Both ids come from the
    // `PreflightReport` the stage-1 checks already produced — the GraphQL
    // lookup that resolved them happened there, and this reuses it.
    //
    // The binding is `preflight`, shadowing the module name inside this
    // function (`let preflight = preflight::run(...)` above). That is the
    // existing style here; do not rename it, and do not invent a `report` —
    // the module path still resolves because the calls below are written
    // `crate::preflight::` where the shadow would bite.
    let expected_identity = crate::preflight::withdrawal_identity_frs(
        &preflight.bridge_dapp_id,
        &preflight.bridge_account_id,
    );
    crate::preflight::check_bridge_deploy(
        &args.rpc_url,
        args.bridge_address,
        // Read from `args`, NOT from `plumbing`: `plumbing` is `None` for
        // every dry run (`let plumbing = if dry_run { None }`), so sourcing
        // it there would make the "pass --verifiers-dir to a dry run"
        // advice unreachable — the flag would be accepted and silently
        // ignored on the one run that has time to use it.
        args.verifiers_dir.as_deref(),
        expected_identity,
        &amount,
    )
    .await
    .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;
    info!(bridge = %args.bridge_address, "bridge deploy ok");

    // Signer + prover artifacts — real runs only (a dry-run has no
    // plumbing, never submits and never proves).
    //
    // The proving key's identity as of this check travels to stage 5. That
    // is the ONLY place its ~2.65 GB of bytes are ever verified, and stage
    // 5 happens after the burn plus up to ~91 minutes of anchor wait — see
    // [`crate::preflight::PkFingerprint`].
    let mut pk_fingerprint: Option<crate::preflight::PkFingerprint> = None;
    if let Some(p) = plumbing.as_ref() {
        crate::preflight::parse_eth_signer(&p.eth_private_key)
            .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;
        info!("burner key ok");
        pk_fingerprint = crate::preflight::check_prover_artifacts(
            p,
            &args.snark_dir,
            args.pk_cache_dir.as_deref(),
            args.allow_verifier_drift,
        )
        .await
        .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;
        info!(params_dir = %p.params_dir.display(), "prover artifacts ok");
    }

    // ---- 2. Confirm, then compose (both strictly before any state write) ----
    //
    // Ordering is load-bearing. The confirmation and every fallible
    // pre-send step run BEFORE the reservation, so a declined prompt or a
    // bad keys.json leaves nothing on disk and the identical command can
    // simply be re-run. Between the reserve and `burn::send` there is
    // exactly ONE thing allowed to fail — `BurnPermit::issue`, whose
    // whole job is to stop a run that has lost the withdrawal lock — and
    // its refusal names the reservation it leaves behind. Anything else
    // added there is a new way to strand an identity; see burn.rs.
    //
    // `state_dir` and `prior` were bound in stage 1 (see the balance-check
    // note there); `prior` is read-only, so reaching this point has not
    // created anything.
    let bounce = true; // spec default; see burn.rs header comment

    // `--dry-run` still returns HERE, before stages 3-6. The early return
    // below is not part of this block — folding dry-run into the `if` as a
    // `("<dry-run>", bounce)` arm looks tidier and is wrong: execution then
    // falls through to capture, prove and submit carrying a fake
    // transaction hash, and a dry run starts waiting on an anchor bundle
    // for an event that was never emitted.
    let mut record = None;
    // Held for the REST OF THE RUN, not just past the burn.
    //
    // Scoping it to the reservation-and-send block was enough for the
    // double-burn it was added for, and not enough for what comes after:
    // two runs that both get past the burn reach stage 6 together, and the
    // loser's `withdrawByProof` reverts on the nullifier and writes
    // `Failed` over the winner's `Confirmed`. The record then says a
    // paid-out withdrawal is resumable.
    //
    // `_`-prefixed but a real binding — `let _ = ..` would drop the guard
    // on the spot and the exclusion would silently disappear.
    let mut _withdrawal_lock: Option<idempotency::WithdrawalLock> = None;
    let (an_tx_hash, bounce) = if dry_run {
        info!("stage 2/6: idempotency (skipped for --dry-run)");
        // Unreachable past the early return below; present only so both
        // arms type-check.
        ("<dry-run>".to_string(), bounce)
    } else {
        // `preflight::run` built one of these from the same endpoint at
        // its first step, so reaching a failure here means the same
        // construction succeeded once and then stopped — resource
        // exhaustion, in practice. Either way nothing has been broadcast,
        // which is why this shares preflight's constructor rather than
        // keeping a byte-identical copy that mapped the failure to exit 10
        // — "the USDC has left the source multisig regardless", about a
        // run that had not composed a message yet.
        let context = preflight::build_client_context(&args.gql_endpoint)
            .map_err(|e| refusal_before_a_recorded_burn(e, recorded_an_tx))?;

        // Resume: a prior record carrying an an_tx_hash means the burn was
        // already broadcast at least once. Reuse it; never compose a second
        // message — the multisig has no replay guard (sendTransaction
        // happily authorises a second transfer).
        if let Some(p) = prior.as_ref().filter(|p| p.an_tx_hash.is_some()) {
            info!(
                an_tx = ?p.an_tx_hash,
                prior_status = ?p.status,
                "stage 3/6: resume — skipping burn (prior an_tx_hash on file)",
            );
            // The hash the rest of the pipeline captures against is the
            // one the reservation returned, not the one peeked above.
            // Carrying the peek's forward here put back the exact
            // disagreement this seam exists to eliminate, one call along.
            let (r, an_tx) = resume_recorded_burn(
                &state_dir,
                &from,
                &to,
                &amount,
                args.allow_retry,
                p,
                &mut _withdrawal_lock,
            )?;
            // From here to the end of this branch, same guarantee as the
            // burn arm gets from its permit. The resume path never sends,
            // so there is no permit to carry it — but everything below
            // still runs for up to ~101 minutes with a concurrent run's
            // liveness probe asking whether this one is alive.
            let _held = _withdrawal_lock
                .as_ref()
                .map(idempotency::WithdrawalLock::hold);
            record = Some(r);
            (an_tx, bounce)
        } else {
            // A prior record with no `an_tx_hash` is the ambiguous case, and
            // it has to be classified HERE — before the prompt. `reserve`
            // would refuse it correctly, but only after
            // `confirm_before_burn` has already run, so a plain retry
            // without `--yes` exits 2 ("stdin is not a TTY") and the
            // operator never learns that a possibly-in-flight burn is what
            // is actually blocking them. Exit 3 with the real reason is the
            // whole point of the code.
            //
            // This creates no RECORD: `peek` is read-only, and a refusal
            // that reserved would be the duplicate it is refusing. It is
            // not quite "leaves the directory as it found it", and saying
            // so would be a lie an operator could check: the liveness
            // probe below opens `<key>.lock` with O_CREAT, so an empty
            // lock file can appear here. It carries no state — the lock
            // lives in the kernel, not in the bytes — and the next run
            // reuses it.
            if let Some(p) = prior.as_ref() {
                // The SAME refusal the post-reservation check produces,
                // and `--allow-retry` no longer changes it.
                //
                // Two things were wrong with the old shape. It emitted
                // `DuplicateInFlight`, whose text advises passing
                // `--allow-retry` — which lands on a DIFFERENT exit 3 and
                // helps nobody — and whose `prior_tx` is structurally
                // `None` here, printing "Prior AN tx: None" for a record
                // that may well be a burn in flight. And the recovery
                // procedure tells an operator to re-run the identical
                // command and read the refusal, which is precisely the
                // command that got the uninformative one.
                //
                // With the flag it warned and carried on, only to be
                // refused after the prompt and `compose` by
                // `decide_burn(None, Found)`. Same answer, later, after
                // work. The one path this does NOT close is the intended
                // recovery: an operator who has reconciled and deleted
                // the record sees `peek` return `None` and never arrives
                // here at all.
                return Err(CliError::ReservationInFlight {
                    prior_status: format!("{:?}", p.status).to_ascii_lowercase(),
                    prior_msg_id: p.withdrawal_msg_id.clone(),
                    record_path: idempotency::record_path(&state_dir, &p.key)
                        .display()
                        .to_string(),
                    // The half the record cannot supply. This run holds
                    // no lock — it has not reserved — so the probe is
                    // asking about somebody else.
                    liveness: idempotency::liveness_verdict(
                        idempotency::WithdrawalLock::probe_holder(&state_dir, &p.key),
                    )
                    .to_string(),
                });
            }

            // The last reversible moment. `--yes` skips the prompt for
            // scripts; `--non-interactive` without `--yes` is already
            // refused in `main::dispatch` (a policy check there, not a clap
            // conflict), so a live prompt here is safe to block on.
            if !skip_prompt {
                confirm_before_burn(&from, &to, &amount, &args, anchor_mode)?;
            }

            info!("stage 3/6: burn (multisig sendTransaction → USDCBridge.initiateWithdrawal)");
            let composed = burn::compose(
                &context,
                &preflight,
                &from,
                &args.from_keys,
                &to,
                &amount,
                bounce,
            )
            .await?;

            // Point of no return starts on the next line.
            info!("stage 2/6: idempotency reserve");
            let (r, decision, lock) =
                reserve_and_decide(&state_dir, &from, &to, &amount, args.allow_retry)?;
            _withdrawal_lock = lock;
            // Before the match, so BOTH arms are covered. `Reuse` issues
            // no permit — it is the "another run broadcast while this one
            // preflighted, resume at capture" case, which is exactly when
            // two runs are working the same identity — and `Send`'s
            // permit borrow ends with its arm, leaving the tail of this
            // branch on convention alone.
            let _held = _withdrawal_lock
                .as_ref()
                .map(idempotency::WithdrawalLock::hold);
            record = Some(r);

            match decision {
                BurnDecision::Reuse(existing) => {
                    // `composed` is dropped here without being sent, which
                    // also drops the owner `KeyPair` it holds — the same
                    // discipline `send` would have applied on return.
                    warn!(
                        an_tx = %existing,
                        "another run reserved this withdrawal and broadcast it while this one was \
                         preflighting; reusing its AN tx and resuming at capture rather than \
                         burning a second time. Nothing extra was sent.",
                    );
                    (existing, bounce)
                },
                BurnDecision::Send => {
                    // The last reversible moment, and the only one that
                    // checks rather than assumes. `issue` asks the kernel
                    // whether this run still owns the identity it
                    // reserved, and the permit it returns BORROWS the
                    // lock — so from here to the end of this arm the
                    // compiler refuses to let anything reassign, move or
                    // drop `_withdrawal_lock`.
                    //
                    // "To the end of this arm" is true only because
                    // `BurnPermit` implements `Drop`. Without it the
                    // borrow would end at the permit's last use, the
                    // `send` call below — and the `update` a few lines
                    // down, which writes the AN tx hash to disk, would be
                    // outside it. That write is the exact window a
                    // concurrent run's liveness probe asks about.
                    let permit = idempotency::BurnPermit::issue(
                        &state_dir,
                        &idempotency::key(&from, &to, &amount),
                        _withdrawal_lock.as_ref(),
                    )?;
                    info!(
                        locked = permit.holds_a_lock(),
                        "stage 3/6: broadcasting the burn",
                    );
                    let receipt = burn::send(&context, &from, composed, &permit).await?;
                    // The receipt's own amount, not the requested one:
                    // this line is what an operator reconciles against,
                    // and it should say what went on the wire. It also
                    // makes the field load-bearing — clippy reported it
                    // as never read, which was true and was the bug in
                    // the log line rather than in the receipt.
                    info!(
                        an_tx = %receipt.an_tx_hash,
                        amount_micro = receipt.sent_amount_micro,
                        "burn broadcast",
                    );
                    if let Some(r) = record.as_mut() {
                        r.status = Status::Burned;
                        r.an_tx_hash = Some(receipt.an_tx_hash.clone());
                        // `?` would be wrong here, and quietly so. `update` reports
                        // its failures as `CliError::Preflight` — exit 2, whose
                        // published meaning is "refused before sending, nothing
                        // left the machine". The burn is on the wire. Exit 2 tells
                        // an operator, and every script parsing the contract, the
                        // opposite of the truth, and the hash never reaches the
                        // JSON output because the error path carries no fields.
                        //
                        // Reclassify, and put the hash where it can be read: this
                        // is exactly `BurnOutcomeUnknown` — sent, outcome not
                        // durably recorded.
                        if let Err(e) = idempotency::update(&state_dir, r) {
                            return Err(e.after_send(&format!(
                                "the AN burn was broadcast as {}, and to resume you must write \
                                 that hash into the record's an_tx_hash and set status to \
                                 \"burned\" before re-running with --allow-retry",
                                receipt.an_tx_hash,
                            )));
                        }
                    }
                    (receipt.an_tx_hash, receipt.bounce)
                },
            }
        }
    };

    // The lock is settled: whichever branch above ran, what this process
    // holds will not change again. Say that to the compiler rather than
    // to the next reader.
    //
    // The HOLD is what does the work, and this comment claimed otherwise
    // for two rounds. Measured: with the rebinding removed, the hold
    // alone still refuses `= None` (E0506), `.take()` (E0502) and
    // `drop(..)` (E0505) — a value with a `Drop` impl is live to the end
    // of its scope, which is what pins the borrow open. Dropping the HOLD
    // does not release the lock, so there is no one-line way back out.
    //
    // The immutable rebinding is kept for the diagnostic, not the
    // guarantee: `= None` then reports E0384, "cannot assign twice",
    // which says what to do about it. The two holds above have no
    // rebinding and are no weaker for it.
    //
    // The three of them were the escape this guard has been chasing for
    // four rounds: stages 4-6 run for up to ~101 minutes, and a second
    // run asking "is anybody executing this withdrawal?" is answered by
    // the kernel holding this descriptor, not by anything on disk. Lose
    // it here and the concurrent run is told the record was left by a run
    // that already exited — which the runbook turns into permission to
    // delete it.
    //
    // ABOVE the dry-run return, not below it. Placed below, the stretch
    // between the burn branch closing and this line was covered by
    // nothing — the permit's borrow ends with its own arm. A dry run
    // holds no lock, so the `map` yields `None` and the early return is
    // unaffected.
    let _withdrawal_lock = _withdrawal_lock;
    let _lock_held_to_the_end = _withdrawal_lock
        .as_ref()
        .map(idempotency::WithdrawalLock::hold);

    // Full `--dry-run`: stop before touching either chain. Returning a
    // stub success record keeps the output path uniform.
    if dry_run {
        info!("dry-run: skipping burn / capture / prove / submit");
        return Ok(WithdrawSuccess {
            burn: BurnSummary {
                an_tx: "<dry-run>".into(),
                bounce: true,
                amount: amount.display(),
            },
            capture: CaptureSummary {
                withdrawal_msg_id: "<dry-run>".into(),
                block_seq_no: 0,
                block_id: "<dry-run>".into(),
                envelope_hash: "<dry-run>".into(),
            },
            proof: ProofSummary {
                self_verified: false,
                calldata_bytes: 0,
                pi_count: 0,
            },
            submit: SubmitSummary {
                eth_tx: None,
                status: SubmitStatus::DryRunOk,
            },
        });
    }

    // Past the dry-run return, so `plumbing` is `Some` by construction —
    // it is resolved unconditionally for every non-dry run above.
    let plumbing_ref = plumbing.as_ref().expect("non-dry-run resolves plumbing");

    // ---- 4. Capture WithdrawalInitiated ----
    // Split the "dapp_id::account_id" the preflight resolved for USDCBridge
    // back into its two halves — the capture helper wants them separately.
    //
    // Exit 12 for an internal invariant, which reads oddly until you ask
    // what exit 2 would mean here: nothing broadcast, nothing written.
    // Both false — this line is downstream of `burn::send` and of the
    // record that names its hash. The stage is what picks the code, never
    // the kind of failure, and the same reasoning applies to every line
    // below this one. `no_refusal_after_the_send_claims_to_be_pre_send`
    // is what keeps it that way.
    let (bridge_dapp_id_hex, bridge_account_id_hex) =
        split_extended(&preflight.usdc_bridge_extended).ok_or_else(|| CliError::ProofFailed {
            reason: format!(
                "internal: usdc_bridge_extended {} is not `dapp_id::account_id`. The AN burn {} \
                 IS on the wire — reconcile it before re-running.",
                preflight.usdc_bridge_extended, an_tx_hash,
            ),
            source: None,
        })?;

    info!("stage 4/6: capture WithdrawalInitiated event (targeted by an_tx_hash)");
    let gql = bridge_gql_fetcher::gql_client::create_client(&args.gql_endpoint).map_err(|e| {
        CliError::ProofFailed {
            reason: format!("failed to build GqlClient for {}: {e}", args.gql_endpoint),
            source: Some(e),
        }
    })?;
    // Multi-user-safe capture: chain-walk from the multisig tx hash we
    // just broadcast to the ExtOut USDCBridge emits when it processes the
    // internal message. This never picks up somebody else's concurrent
    // burn because the initial filter is `transaction(hash: an_tx_hash)`,
    // which by construction is only satisfied by our own broadcast.
    let captured = bridge_relayer_daemon::withdraw_e2e::capture_targeted_withdrawal_event(
        &gql,
        &an_tx_hash,
        &preflight.usdc_bridge_legacy,
        DEFAULT_EVENT_DST,
        &bridge_account_id_hex,
        &bridge_dapp_id_hex,
        EVENT_WAIT,
        EVENT_POLL_INTERVAL,
    )
    .await
    // Capture failures are their own stage (exit 11) — a timeout here
    // means "AN burn broadcast, event never observed", which is a
    // reconcile-and-resume situation, not a prover crash. The prior
    // catch-all `ProofFailed` mapping (exit 12) misled operators into
    // treating this as a Circuit-4 problem.
    .map_err(|e| CliError::CaptureTimeout {
        an_tx: format!("{an_tx_hash} ({e})"),
    })?;
    info!(
        msg_id = %captured.message_id,
        block_seq_no = captured.block_seq_no,
        block_id = %captured.block_id_hex,
        "captured WithdrawalInitiated event",
    );
    if let Some(r) = record.as_mut() {
        r.status = Status::Captured;
        r.withdrawal_msg_id = Some(captured.message_id.clone());
        r.block_seq_no = Some(captured.block_seq_no);
        idempotency::update(&state_dir, r).map_err(|e| {
            e.after_send(
                "the AN burn is on the wire and its WithdrawalInitiated event was captured",
            )
        })?;
    }

    // ---- 4b. Resurrect BridgeState from contract; wait for coverage ----
    info!("stage 4b/6: resurrect BridgeState from AckiNackiBridge + wait for covering bundle");
    let stride = stride_for(anchor_mode);
    let target_covering_seq_no = covering_bundle_seq_no(captured.block_seq_no, stride);
    info!(
        burn_seq_no = captured.block_seq_no,
        anchor_stride = stride,
        target_covering_seq_no,
        "waiting for on-chain coverage",
    );
    let ro_provider = ProviderBuilder::new().connect_http(args.rpc_url.parse().map_err(|e| {
        CliError::EthSubmitFailed {
            reason: format!("--rpc-url is not a valid URL: {e}"),
            source: None,
        }
    })?);
    let ro_bridge_for_wait = EthBridgeClient::new(args.bridge_address, ro_provider);
    let bridge_state = wait_for_coverage(
        &ro_bridge_for_wait,
        anchor_mode,
        target_covering_seq_no,
        COVERAGE_POLL_INTERVAL,
        COVERAGE_WAIT,
    )
    .await
    .map_err(|e| CliError::ProofFailed {
        reason: format!("wait_for_coverage: {e}"),
        source: Some(e),
    })?;

    // ---- 5. Prove ----
    info!("stage 5/6: Circuit-4 SHPLONK proof (in-process C4 → aggregator subprocess)");

    // Before anything reads the key. Stage 1 verified it; the burn and the
    // anchor wait sit between then and now, and nothing else re-checks —
    // `keys_cached()` asks only whether the file EXISTS. Cheap enough to do
    // unconditionally, and the refusal it produces is strictly better than
    // proving against an unvouched-for key and being rejected on submit.
    crate::preflight::recheck_proving_key(pk_fingerprint.as_ref())?;
    // `prover_state_path` and `window_size` are ignored by
    // `run_once_with_state`, but the config struct still has the fields
    // (the daemon binary uses them on the file path). Fill in stubs
    // that make the intent clear if anything ever accidentally reads
    // them.
    let e2e_cfg = WithdrawE2EConfig {
        gql_endpoint: args.gql_endpoint.clone(),
        prover_state_path: PathBuf::new(),
        window_size: 0,
        bridge_account_id_hex,
        bridge_dapp_id_hex,
        event_dst_filter: DEFAULT_EVENT_DST.to_string(),
        event_wait: EVENT_WAIT,
        event_poll_interval: EVENT_POLL_INTERVAL,
        anchor_mode,
        i_know_the_wait: args.i_know_the_wait,
        work_dir: plumbing_ref.work_dir.clone(),
        aggregator_dir: plumbing_ref.aggregator_dir.clone(),
        verifiers_dir: plumbing_ref.verifiers_dir.clone(),
        params_dir: plumbing_ref.params_dir.clone(),
        snark_dir: args.snark_dir.clone(),
        pk_cache_dir: args.pk_cache_dir.clone(),
        prover_out_dir: args.prover_out_dir.clone(),
        prover_timeout: Duration::from_secs(args.prover_timeout_s),
        // The event's own block seq_no, not a constant.
        //
        // It is stamped into `event_{:06}_witness.json` and
        // `proof_event_{:06}.json` (`withdraw_e2e/driver.rs`), so a
        // hard-coded 0 named every run's files identically: two
        // withdrawals through one `--work-dir` silently overwrote each
        // other's witness, which the runbook simultaneously told operators
        // to keep because regenerating it is expensive. It also made every
        // documented `proof_event_<seq>.json` path wrong — there was only
        // ever `proof_event_000000.json`.
        //
        // `u32`, and the capture's seq_no is `u64`. A saturating cast, not
        // `as`: `as` wraps, and a wrapped seq_no is a filename that
        // collides with a real one rather than an obviously wrong number.
        prover_seq_no: u32::try_from(captured.block_seq_no).unwrap_or(u32::MAX),
        replay_latest: true,
    };
    let e2e = run_once_with_state(e2e_cfg, bridge_state, captured)
        .await
        .map_err(|e| CliError::ProofFailed {
            reason: format!("withdraw-e2e pipeline failed: {e}"),
            source: Some(e),
        })?;
    let calldata_bytes = e2e.proof.proof_hex.len() / 2;
    let pi_count = e2e.proof.public_instances_hex.len();
    info!(
        msg_id = %e2e.captured.message_id,
        block_seq_no = e2e.captured.block_seq_no,
        block_id = %e2e.captured.block_id_hex,
        calldata_bytes,
        pi_count,
        self_verified = e2e.proof.self_verified,
        "capture + prove complete",
    );
    if let Some(r) = record.as_mut() {
        r.status = Status::Proved;
        r.withdrawal_msg_id = Some(e2e.captured.message_id.clone());
        r.block_seq_no = Some(e2e.captured.block_seq_no);
        idempotency::update(&state_dir, r).map_err(|e| {
            e.after_send("the AN burn is on the wire and the Circuit-4 proof is complete")
        })?;
    }

    // ---- 6. Submit (dry-run then real) ----
    info!("stage 6/6: submit withdrawByProof");
    let proof_bytes = e2e
        .proof
        .proof_bytes()
        .map_err(|e| CliError::EthSubmitFailed {
            reason: format!("PartnerWithdrawalProof::proof_bytes: {e}"),
            source: Some(anyhow::Error::new(e)),
        })?;
    let pub_inputs = e2e
        .proof
        .public_inputs()
        .map_err(|e| CliError::EthSubmitFailed {
            reason: format!("PartnerWithdrawalProof::public_inputs: {e}"),
            source: Some(anyhow::Error::new(e)),
        })?;

    // Always dry-run first — catches on-chain-side issues (paused bridge,
    // treasury shortfall) before we spend gas.
    {
        let ro_provider =
            ProviderBuilder::new().connect_http(args.rpc_url.parse().map_err(|e| {
                CliError::EthSubmitFailed {
                    reason: format!("--rpc-url is not a valid URL: {e}"),
                    source: None,
                }
            })?);
        let ro_bridge = EthBridgeClient::new(args.bridge_address, ro_provider);
        match ro_bridge
            .dry_run_withdraw(&proof_bytes, &pub_inputs)
            .await
            .map_err(|e| CliError::EthSubmitFailed {
                reason: format!("dry_run_withdraw: {e}"),
                source: Some(anyhow::Error::new(e)),
            })? {
            DryRunOutcome::WouldSucceed => info!("eth dry-run: withdrawByProof would succeed"),
            DryRunOutcome::WouldRevert {
                reason,
            } => {
                return Err(CliError::EthSubmitFailed {
                    reason: format!("withdrawByProof dry-run reverted: {reason}"),
                    source: None,
                });
            },
        }
    }

    // Real submit: build a wallet-filled provider and send.
    //
    // Both the key shape and the chain id were settled in stage 1, before
    // the burn — `preflight::parse_eth_signer` and
    // `preflight::check_destination_chain` — and re-asking the RPC for its
    // chain id would only let a load-balanced endpoint disagree with
    // itself between stage 1 and stage 6.
    //
    // The paragraph that stood here argued against re-parsing the key,
    // and the line below it re-parsed the key. Worth keeping the reason
    // it was wrong: it said the cost would be "exit 13 for something that
    // never reached the wire", while what it actually shipped was exit 2
    // — nothing broadcast, no state file written — for a run whose AN
    // burn is on the wire and whose record is on disk. Exit 13 names the
    // stage that could not complete, which is the truth here; exit 2
    // names a state this run left behind an hour ago.
    //
    // The parse itself stays. It cannot fail — stage 1 parsed the same
    // string — and the point is what it costs WHEN THE IMPOSSIBLE
    // HAPPENS, which is the only thing an exit code is read for.
    let signer = preflight::parse_eth_signer(&plumbing_ref.eth_private_key).map_err(|e| {
        CliError::EthSubmitFailed {
            reason: format!(
                "internal: --eth-private-key parsed in stage 1 and not in stage 6: {e}\n\x20 The \
                 AN burn IS on the wire and the proof is built. Nothing was submitted to the EVM \
                 chain; re-running with --allow-retry resumes from the recorded burn.",
            ),
            source: None,
        }
    })?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(to.chain_id)));
    let provider =
        ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(
                args.rpc_url
                    .parse()
                    .map_err(|e| CliError::EthSubmitFailed {
                        reason: format!("--rpc-url parse: {e}"),
                        source: None,
                    })?,
            );
    let bridge = EthBridgeClient::new(args.bridge_address, provider);

    // NB: `Status::Submitted` is written only AFTER `submit_withdraw`
    // returns with an actual `tx_hash`. Writing it beforehand (as v1
    // originally did) was misleading — if `submit_withdraw` errored out
    // before broadcast (RPC unreachable, wallet reject, gas estimation
    // failure), the on-disk record would falsely claim a tx was in
    // flight, and the next run would refuse-duplicate on Submitted
    // instead of allowing a retry.
    let submit = match bridge
        .submit_withdraw(&proof_bytes, &pub_inputs)
        .await
        .map_err(|e| CliError::EthSubmitFailed {
            reason: format!("submit_withdraw: {e}"),
            source: Some(anyhow::Error::new(e)),
        })? {
        WithdrawSubmitOutcome::Paid {
            tx_hash,
        } => {
            let tx = format!("{tx_hash:?}");
            info!(tx = %tx, "withdrawByProof paid out");
            // Preserve the two-step trail (Submitted → Confirmed) so debug
            // tools and crash recovery can distinguish "we broadcast" from
            // "we saw the receipt". Both writes happen post-broadcast, so
            // the state file never over-claims.
            if let Some(r) = record.as_mut() {
                r.status = Status::Submitted;
                r.eth_tx_hash = Some(tx.clone());
                idempotency::update(&state_dir, r).map_err(|e| {
                    e.after_send(&format!(
                        "withdrawByProof paid out on the EVM side as {tx} — the USDC has MOVED on \
                         both chains"
                    ))
                })?;
            }
            SubmitSummary {
                eth_tx: Some(tx),
                status: SubmitStatus::Confirmed,
            }
        },
        WithdrawSubmitOutcome::Reverted {
            reason,
        } => {
            // Keep the record so a follow-up run resumes here instead of
            // burning again. The proof itself is NOT kept: it is
            // regenerated, which is safe because it is deterministic per
            // (event, prover_state).
            //
            // This used to say the record carried a `proof_json_path`
            // "populated by the aggregator subprocess if the operator
            // passed `--prover-out-dir`". The flag is real and does write
            // `<dir>/proof_event_<seq>.json`, but nothing ever put that
            // path on the record and nothing ever read it back, so the
            // field is gone and re-proving is the only path there was.
            if let Some(r) = record.as_mut() {
                r.status = Status::Failed;
                // Deliberately not propagated, and the only `update` in
                // this file that is not. We are already returning
                // `EthSubmitFailed` (exit 13), which names the revert and
                // is strictly more useful than "we also could not write it
                // down"; replacing it with an `after_send` refusal would
                // hide the revert reason behind a bookkeeping failure. The
                // record staying at `Proved` is safe — it still blocks a
                // plain retry.
                //
                // Not propagated is not the same as unsaid, which is what
                // it used to be. An operator reading this run's output
                // would otherwise see exit 13, go to the record to find
                // `Failed`, and find `Proved` with nothing anywhere
                // explaining the difference.
                if let Err(e) = idempotency::update(&state_dir, r) {
                    warn!(
                        error = %e.after_send("the withdrawal reverted on chain"),
                        "could not mark the record failed after the revert; it stays at `proved`, \
                         which still refuses a plain retry. The exit code and the revert reason \
                         below are the authoritative outcome",
                    );
                }
            }
            return Err(CliError::EthSubmitFailed {
                reason: format!("withdrawByProof reverted: {reason}"),
                source: None,
            });
        },
    };

    if let Some(r) = record.as_mut() {
        r.status = Status::Confirmed;
        r.eth_tx_hash = submit.eth_tx.clone();
        idempotency::update(&state_dir, r).map_err(|e| {
            e.after_send("the withdrawal completed on both chains and its payout receipt was seen")
        })?;
    }

    Ok(WithdrawSuccess {
        burn: BurnSummary {
            an_tx: an_tx_hash,
            bounce,
            amount: amount.display(),
        },
        capture: CaptureSummary {
            withdrawal_msg_id: e2e.captured.message_id,
            block_seq_no: e2e.captured.block_seq_no,
            block_id: e2e.captured.block_id_hex,
            envelope_hash: e2e.captured.envelope_hash_hex,
        },
        proof: ProofSummary {
            self_verified: e2e.proof.self_verified,
            calldata_bytes,
            pi_count,
        },
        submit,
    })
}

// ---- helpers ----

/// Whether stage 3 must actually broadcast.
///
/// Decided from the record [`idempotency::reserve`] returned, **not** from
/// the `peek` taken back in stage 1. Those are different answers, and the
/// gap between them is a second irreversible burn.
///
/// `reserve` is the atomic point: it is the `hard_link` that either wins
/// the identity or reads whatever is already there. `peek` happens much
/// earlier — before the whole EVM preflight, before the confirmation
/// prompt, before `compose` — and with an interactive prompt that window is
/// unbounded. Another run on this host can reserve, burn, and record its
/// hash inside it.
///
/// With `--allow-retry`, `reserve` then hands that run's record back
/// (`Ok(prior)` for `Reserved | Burned | Captured | Proved`), and it carries
/// `an_tx_hash`. Without this check the caller stored that record and sent
/// anyway — a second `initiateWithdrawal` for a withdrawal already on the
/// wire, against a multisig with no replay guard.
///
/// Without `--allow-retry` the same race is already safe: `reserve` returns
/// `DuplicateInFlight` and the run exits 3. So this is specifically the
/// `--allow-retry` path, which is exactly the path that exists to be used
/// after something went wrong — i.e. when a second operator is most likely
/// to be poking at the same withdrawal.
#[derive(Debug, PartialEq, Eq)]
enum BurnDecision {
    /// No hash on the reserved record: nothing has reached the wire for
    /// this identity, and this run is the one that broadcasts.
    Send,
    /// The reserved record already names a broadcast burn. Reuse its hash
    /// and resume at capture; do not compose a second message.
    Reuse(String),
}

/// Claim the identity for a withdrawal whose burn is ALREADY on the wire,
/// and hand back a record that says so.
///
/// The resume path used to call `reserve` directly and discard its answer,
/// reasoning that a resume sends nothing so the decision cannot matter.
/// Two things followed, neither about sending. It took no
/// `WithdrawalLock`, so a resuming run was invisible to the liveness
/// probe the exit-3 refusal promises and two concurrent resumes both
/// reached the submit stage — where the loser's revert writes `Failed`
/// over the winner's `Confirmed`. And it ignored the reservation, so a
/// record deleted between the stage-1 `peek` and here left the run
/// carrying on with the peeked hash and never writing it back: stage 4
/// then persisted `Captured` with `an_tx_hash: None`, which `read_record`
/// refuses forever as "acting on it would broadcast a SECOND burn". A
/// completed withdrawal and an unrecoverable record.
///
/// `observed` is that evidence — the record as stage 1 read it moments ago.
/// It is what makes the `Send` arm below safe to convert into a restore
/// rather than a refusal.
///
/// The WHOLE record, not just its hash. Restoring a fixed `Burned` was the
/// next layer of the same defect: this branch is entered on `an_tx_hash`
/// alone, so a `confirmed` record — a withdrawal that has already paid out
/// — deleted during the minutes-long preflight came back as `burned` and
/// was carried through capture, prove and a SECOND `withdrawByProof`.
/// `reserve` refuses that record; it just never saw it, because the file
/// was gone.
///
/// `hold` receives the withdrawal lock, and is the reason it is a
/// parameter rather than a third element of the return tuple.
///
/// Returned, the lock sat in a slot a caller could write `_` into. That
/// compiles, passes clippy, passes every test, and drops the guard on the
/// spot — `_` is not a binding, so it does not live to the end of the
/// scope the way `_withdrawal_lock` does. `#[must_use]` does not reach
/// inside a tuple pattern, and neither does
/// `clippy::let_underscore_must_use`, which only sees `let _ = expr`. The
/// run then walks into `burn::send` holding nothing, invisible to the
/// liveness probe every recovery procedure in the runbook depends on.
///
/// Writing through `hold` removes that slot. It does NOT make the lock
/// unloseable, and the previous version of this paragraph said it did:
/// `&mut None` at the call site discards it just as quietly, and so does
/// any `= None`, `.take()` or `drop()` further down the run. What the
/// out-parameter buys is one specific evasion closed and a call site a
/// grep can pin.
///
/// The send is defended by something stronger, because that is where the
/// money is: [`idempotency::BurnPermit`] borrows the lock across
/// `burn::send`, so between the check and the broadcast the compiler
/// refuses every one of those. This path never sends — it exists because
/// a burn is already on the wire — so it keeps the weaker guarantee, and
/// what the lock still buys here is the liveness answer a concurrent run
/// gets while this one finishes capture, prove and submit.
///
/// It is stored before the first fallible step, so the caller holds it on
/// the error paths too — on `Err` the run aborts and the kernel releases
/// it, which is what should happen, and the record-restoring writes below
/// happen under it either way.
fn resume_recorded_burn(
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    allow_retry: bool,
    observed: &idempotency::Record,
    hold: &mut Option<idempotency::WithdrawalLock>,
) -> CliResult<(idempotency::Record, String)> {
    // Every refusal raised anywhere inside this function is a post-send
    // refusal, and this is the FIRST line of it — not just the `reserve`
    // further down. `reserve_and_decide` reaches four separate
    // `CliError::Preflight` sources before it returns: preparing the state
    // directory, opening the withdrawal lock, the `peek` behind a
    // contended one, and `reserve` itself. Each of them says "nothing was
    // sent" about a run whose entry condition is a burn already on the
    // wire.
    let (r, decision, lock) = reserve_and_decide(state_dir, from, to, amount, allow_retry)
        .map_err(|e| resumed_refusal(e, observed))?;
    *hold = lock;
    match decision {
        // The hash comes from the RESERVATION, not from `observed`. The
        // two are read minutes apart — the whole of preflight — and the
        // reservation's is the one taken under the withdrawal lock. An
        // operator who followed the exit-3 remedy and wrote the real hash
        // into the record in between is the case that makes them differ,
        // and it is the case the remedy exists for.
        BurnDecision::Reuse(h) => Ok((r, h)),
        // A FRESH reservation for an identity this run has just seen a
        // burn recorded against: the record was removed in between.
        //
        // `Send` is the right general answer for a record with no hash,
        // and the wrong one here — this branch composed nothing to send,
        // and it holds evidence the burn happened. Restore the hash into
        // the record we now own rather than leaving behind the one no
        // later run can act on.
        BurnDecision::Send => {
            // Only the caller's peek says a burn happened — the record it
            // saw is gone. Without a hash there is nothing to restore and
            // nothing to resume, and this arm exists only because the
            // caller saw one.
            warn!(
                an_tx = ?observed.an_tx_hash,
                prior_status = ?observed.status,
                key = %r.key,
                "the state record was removed while this run was preflighting; restoring it as \
                 stage 1 read it, rather than proceeding on a record that says no burn happened",
            );
            // Verbatim, including the status and `eth_tx_hash`. Writing a
            // fixed status here is what downgraded a paid-out withdrawal,
            // and writing only the status back would still lose the EVM
            // hash an operator needs to reconcile it.
            let restored = idempotency::Record {
                key: r.key.clone(),
                ..observed.clone()
            };
            idempotency::update(state_dir, &restored).map_err(|e| {
                e.after_send(&format!(
                    "the AN burn {} is on the wire — it was recorded before this run started, and \
                     its record has since been deleted",
                    restored.an_tx_hash.as_deref().unwrap_or("<none recorded>"),
                ))
            })?;

            // The file is back, so ask it the whole question the deletion
            // skipped — by asking `reserve`, which is the thing that asks
            // it. Checking only for a terminal status left the rest of the
            // gate out: a `burned` record deleted mid-preflight resumed
            // with no `--allow-retry`, where the identical record still on
            // disk exits 3. Deleting the file is not consent, and it is
            // the CLI's own message that tells operators to delete it.
            //
            // Restore FIRST and ask second — and the reason given here
            // used to be wrong. It said the fresh hash-less reservation
            // left behind by refusing early is one `read_record` "rejects
            // forever". It is not: that guard fires on a hash-less record
            // whose status is `Burned` or later, and this one is
            // `Reserved`, which reads back fine.
            //
            // The ordering is still right, for the reason the wrong
            // sentence was standing in for. What refusing early leaves is
            // a record that says no burn ever happened, sitting on an
            // identity for which one did — so the next run reads it, sees
            // no hash, and is one deleted file away from burning again.
            // Restoring first means the evidence is on disk before
            // anything can refuse, whatever the refusal turns out to be.
            //
            // `reserve` reports its own failures as pre-send refusals,
            // which is correct at its other call site and false at this
            // one. See `resumed_refusal` for what that costs and what is
            // remapped.
            let (reserved, _) = idempotency::reserve(state_dir, from, to, amount, allow_retry)
                .map_err(|e| resumed_refusal(e, observed))?;
            let Some(an_tx) = reserved.an_tx_hash.clone() else {
                // Unreachable by construction — the record just written
                // came from `peek`, so `read_record` has already accepted
                // its shape, and an active status with no hash is one of
                // the shapes it does not. Refuse rather than invent a
                // hash: this is the field a second burn turns on.
                //
                // Exit 10, not 2, and it names the hash rather than the
                // key: this branch is downstream of a burn like every
                // other line in this arm, and the operator's next move is
                // to reconcile that transaction, which they cannot do from
                // a dedup digest.
                return Err(CliError::BurnOutcomeUnknown {
                    reason: format!(
                        "the AN burn {} is on the wire, but the restored record {} came back from \
                         the reservation carrying no AN tx hash, so there is no burn to resume \
                         from. Reconcile that transaction before re-running.",
                        observed.an_tx_hash.as_deref().unwrap_or("<none recorded>"),
                        reserved.key,
                    ),
                    source: None,
                });
            };
            Ok((reserved, an_tx))
        },
    }
}

/// Re-badge a refusal raised inside the resume arm, where the pre-send
/// half of `reserve`'s vocabulary is not available.
///
/// `resume_recorded_burn` is entered only through
/// `.filter(|p| p.an_tx_hash.is_some())`, so a burn is on the wire for
/// EVERY line inside it — from its first statement, not merely from the
/// point where this run has written the record back itself. Exit 2's
/// published contract is that nothing was broadcast, which is false the
/// moment the function is entered, and a retry wrapper reading it fires
/// the second burn.
///
/// Only the pre-send half moves. The exit-3 refusals out of the same call
/// are the entire reason it is made: they carry the per-status remedy, and
/// re-badging them would hide the duplicate this arm exists to catch.
/// Anything else `reserve` might grow passes through untouched rather than
/// being swept into exit 10 by a catch-all.
///
/// The forwarded text is edited, not quoted. `reserve` and `try_acquire`
/// write [`idempotency::NOTHING_SENT_CLAUSE`] into their own refusals, and
/// carrying it through unchanged produced a message that said a burn was
/// on the wire and, two lines below, that nothing had been sent. An
/// operator reconciling a live withdrawal reads whichever half they see
/// first.
/// Re-badge a STAGE 1 refusal when a burn for this identity is already on
/// the wire.
///
/// Nine rounds of review circled this, one site at a time, and the
/// project had already decided it: the resume arm re-badges on the
/// ground that entry is "gated on a burn being on the wire", and a test
/// pins an EISDIR failure there — one that writes nothing and leaves the
/// record intact — at exit 10. `burn_already_sent` is literally the same
/// predicate, computed before preflight and already used to relax the
/// balance check, and six refusals below it kept reporting exit 2 under
/// it.
///
/// The cost is not theoretical. The runbook attaches "do not delete the
/// state file" to exit 10 and tells an exit-2 reader "no state file was
/// written"; both sentences describe the same run, and one of them is
/// wrong. Nothing NEW is broadcast or written by a stage-1 refusal — that
/// half of exit 2's contract holds — but the withdrawal it is refusing
/// has a burn on chain and a record on disk, which is the half an
/// operator acts on.
fn refusal_before_a_recorded_burn(e: CliError, an_tx: Option<&str>) -> CliError {
    let Some(an_tx) = an_tx else {
        return e;
    };
    // Every variant that renders as exit 2. Naming them rather than
    // matching `_` so a variant added later is a compile error here
    // instead of a silent exit-2 escape.
    let reason = match &e {
        CliError::Preflight {
            ..
        }
        | CliError::ArgInvalid {
            ..
        }
        | CliError::KeyFilePerms {
            ..
        }
        | CliError::Usage {
            ..
        } => format!("{e}"),
        _ => return e,
    };
    CliError::BurnOutcomeUnknown {
        reason: format!(
            "preflight refused this run: {reason}\n\x20 Nothing new was sent and nothing new was \
             written — but the AN burn {an_tx} from an earlier run IS on the wire, and its record \
             is on disk. Do not delete that record on the strength of this refusal; fix what \
             preflight named and re-run, which resumes from the recorded burn.",
        ),
        source: None,
    }
}

fn resumed_refusal(e: CliError, observed: &idempotency::Record) -> CliError {
    match e {
        CliError::Preflight {
            reason,
            source,
        } => CliError::BurnOutcomeUnknown {
            reason: format!(
                "the AN burn {} is on the wire — it was recorded before this run started — but \
                 the reservation covering it could not be completed: {}\n\x20 The local record \
                 may be behind the chain. Do not re-run without reconciling — see the runbook's \
                 Case 3a.",
                // Plain, because this is the string an operator pastes
                // into a block explorer. `{:?}` on the `Option` rendered
                // it as `Some("0x…")`, quotes included. The fallback is
                // unreachable through `resume_recorded_burn` — the filter
                // at the call site is what makes this whole function
                // honest — but printing `None` beside "is on the wire"
                // would be the same class of lie this function exists to
                // remove.
                observed.an_tx_hash.as_deref().unwrap_or("<none recorded>"),
                reason.replace(idempotency::NOTHING_SENT_CLAUSE, ""),
            ),
            source,
        },
        other => other,
    }
}

/// Take the reservation, then decide from what it returned.
///
/// A function rather than four lines inline, because the bug this seam
/// exists to close was not in `reserve` and not in [`decide_burn`] — both
/// were right on their own. It was in the WIRING: the caller decided from
/// the stage-1 `peek` instead of from the reservation, and no test of
/// either function could see that. The tests below drive it against a
/// real state directory, including from two threads at once.
///
/// (This paragraph spent several rounds stranded above
/// `resume_recorded_burn`, describing a function two screens away.)
fn reserve_and_decide(
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    allow_retry: bool,
) -> CliResult<(
    idempotency::Record,
    BurnDecision,
    Option<idempotency::WithdrawalLock>,
)> {
    // The lock comes FIRST — before the reservation, long before the send.
    //
    // A lock TAKEN between the reserve and `burn::send` would be a new way
    // to fail with the identity already claimed. Taken here, its only
    // failure is a pre-send refusal like any other. (`BurnPermit::issue`
    // does run down there and can refuse — it re-CHECKS this lock rather
    // than acquiring one, and stopping a run that has lost it is the
    // point. See its rustdoc for what its refusal leaves behind.)
    //
    // Holding it across the send is what lets a LATER run answer the
    // question the record cannot: "is somebody executing this withdrawal
    // right now, or did a run die and leave this behind?" The kernel drops
    // it when the holder exits, so that answer is a fact rather than a
    // guess about wall-clock age.
    let key = idempotency::key(from, to, amount);

    // Before the lock, and propagated. Preparing the state directory and
    // taking the lock are different questions, and letting the first
    // answer the second is what made a failed `mkdir` read as "this
    // filesystem has no flock, carry on unlocked".
    idempotency::ensure_state_dir(state_dir)?;

    let attempt = idempotency::WithdrawalLock::try_acquire(state_dir, &key)?;
    reserve_and_decide_holding(attempt, state_dir, from, to, amount, allow_retry)
}

/// The half of [`reserve_and_decide`] that runs once the lock attempt has
/// been made, split out so a test can supply the attempt.
///
/// `Unsupported` is otherwise unreachable in a test: it needs a filesystem
/// whose `flock` answers `ENOLCK`, and there is no way to conjure one in
/// a `TempDir`. Splitting here is what makes the WIRING testable rather
/// than only the mapping at the far end of it — the gap that let a
/// lockless mount be told nobody held its withdrawal while every test
/// passed.
fn reserve_and_decide_holding(
    attempt: idempotency::LockAttempt,
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    allow_retry: bool,
) -> CliResult<(
    idempotency::Record,
    BurnDecision,
    Option<idempotency::WithdrawalLock>,
)> {
    let key = idempotency::key(from, to, amount);

    // Asked BEFORE the attempt is taken apart, and asked of the attempt
    // rather than restated here.
    //
    // What a refusal downstream is allowed to claim about other processes
    // used to be re-derived from a hand-written `(lock, flock_available)`
    // pair beside the one `LockAttempt::holder_verdict` already computes.
    // Two mappings of the same three cases, one of them under test and one
    // of them not: flipping this arm's `false` to `true` made a lockless
    // mount print "the record was left by a run that has already exited",
    // the row the runbook turns into permission to delete, with the whole
    // suite green. There is now one mapping, and it is the tested one.
    let holder = attempt.holder_verdict();

    let lock = match attempt {
        idempotency::LockAttempt::Held(l) => Some(l),
        // Another live process on this host owns this withdrawal. Refuse
        // before reserving: its outcome is that run's to record, and a
        // second run racing it through capture and submit gets a reverted
        // `withdrawByProof` at best.
        idempotency::LockAttempt::Contended => {
            let prior = idempotency::peek(state_dir, from, to, amount)?;
            return Err(CliError::ReservationInFlight {
                prior_status: prior
                    .as_ref()
                    .map(|p| format!("{:?}", p.status).to_ascii_lowercase())
                    .unwrap_or_else(|| "reserving".to_string()),
                prior_msg_id: prior.and_then(|p| p.withdrawal_msg_id),
                record_path: idempotency::record_path(state_dir, &key)
                    .display()
                    .to_string(),
                // We just failed to take it, so somebody holds it — which
                // is what `holder_verdict` said of this same attempt.
                liveness: idempotency::liveness_verdict(holder).to_string(),
            });
        },
        // `flock` unavailable — and now that means the filesystem cannot
        // do it, not merely that something went wrong. NOT a refusal: a
        // state dir on such a mount is a supported deployment, and the
        // record's own cross-field guards are what hold there. Proceed,
        // and say the evidence is missing if it comes to a refusal.
        //
        // Everything else left through the `?` above, as a refusal with
        // nothing sent. That is the direction to be wrong in: an
        // unnecessary refusal costs a re-run, and continuing unlocked
        // costs a record that a later run is told is safe to delete.
        idempotency::LockAttempt::Unsupported {
            why,
        } => {
            warn!(
                reason = %why,
                "this filesystem does not implement flock; continuing on the record checks alone. \
                 A refusal from this run will say the liveness evidence is missing rather than \
                 claim nobody holds this withdrawal",
            );
            None
        },
    };

    let (r, how) = idempotency::reserve(state_dir, from, to, amount, allow_retry)?;
    info!(key = %r.key, ?how, "reserved");
    // The record AND which side of the atomic publish it came from — NOT
    // the `peek` taken back in stage 1. See [`decide_burn`]: those two
    // disagree in two different ways, and both are a second irreversible
    // burn.
    let decision = decide_burn(&r, how, state_dir, holder)?;
    Ok((r, decision, lock))
}

/// Whether this run broadcasts, decided from what the RESERVATION
/// returned.
///
/// Three inputs and one question: does an `initiateWithdrawal` for this
/// identity still need to go out? The record's `an_tx_hash` answers it
/// when it is set. When it is not, the answer turns on which side of the
/// atomic publish this run came down on — `Created` means the identity is
/// ours and nothing is in flight; `Found` means somebody else's
/// reservation is already there, and the hash it lacks is written only
/// after their send returns, so the record cannot say whether their burn
/// is on the wire. That is the refusal, and `holder` is what the
/// withdrawal lock could add to it.
///
/// It has no doc comment for eight rounds of review, which is how the
/// third liveness verdict got lost twice: the argument for `Option<bool>`
/// over a bool lives in the parameter and nowhere a reader looks first.
///
/// Deliberately NOT given the stage-1 `peek`. The peek and the
/// reservation are read minutes apart, and deciding from the older of the
/// two is a second irreversible burn — see `reserve_and_decide`.
fn decide_burn(
    reserved: &idempotency::Record,
    how: idempotency::Reservation,
    state_dir: &Path,
    // What the lock attempt above said about OTHER processes, carried
    // through unchanged: `Some(false)` this run took the lock so nobody
    // else has it, `None` the filesystem could not answer. Only affects
    // what the refusal is able to claim — a filesystem without `flock` is
    // a supported deployment, not a reason to stop.
    //
    // The attempt's own verdict rather than a bool re-derived from it:
    // `holder_verdict` is where the three cases are mapped, it is under
    // test, and a second mapping beside it is a second thing to get wrong.
    holder: Option<bool>,
) -> CliResult<BurnDecision> {
    match (reserved.an_tx_hash.as_deref(), how) {
        // Someone already broadcast for this identity. Resume at capture.
        (Some(h), _) => Ok(BurnDecision::Reuse(h.to_string())),
        // We won the publish: the identity is ours, nothing is in flight.
        (None, idempotency::Reservation::Created) => Ok(BurnDecision::Send),
        // The record was already there and carries no hash. Two states look
        // identical from here and both forbid sending:
        //
        //  * another run is INSIDE `burn::send` right now and has not come back to write its hash —
        //    send and we double-burn;
        //  * a record was hand-edited to `burned` with a null hash, which the CLI's own post-burn
        //    recovery message invites.
        //
        // `an_tx_hash` cannot distinguish them, because it is written only
        // after the send returns. Refuse, and say what to do.
        (None, idempotency::Reservation::Found) => Err(CliError::ReservationInFlight {
            prior_status: format!("{:?}", reserved.status).to_ascii_lowercase(),
            prior_msg_id: reserved.withdrawal_msg_id.clone(),
            record_path: idempotency::record_path(state_dir, &reserved.key)
                .display()
                .to_string(),
            // The half the record cannot supply, and without which the
            // only escape an operator finds is deleting the guard: this
            // run took the lock before reserving, so no OTHER process can
            // be executing this withdrawal, whatever the record says —
            // unless `flock` never worked here, in which case there is no
            // answer to give and the verdict says so.
            liveness: idempotency::liveness_verdict(holder).to_string(),
        }),
    }
}
/// Parse `--anchor-layer` into an [`AnchorLayerMode`]. Mirrors the relayer
/// bin's `parse_anchor_layer` so the two CLIs share behavior.
fn parse_anchor_layer(s: &str) -> CliResult<AnchorLayerMode> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Ok(AnchorLayerMode::Auto);
    }
    let n: u8 = t.parse().map_err(|_| CliError::ArgInvalid {
        flag: "anchor-layer",
        expected: "auto or a positive integer".into(),
        got: s.to_string(),
    })?;
    if n == 0 {
        return Err(CliError::ArgInvalid {
            flag: "anchor-layer",
            expected: "positive integer or `auto`".into(),
            got: s.to_string(),
        });
    }
    Ok(AnchorLayerMode::Explicit(n))
}

fn split_extended(ext: &str) -> Option<(String, String)> {
    let (a, b) = ext.split_once("::")?;
    Some((a.to_string(), b.to_string()))
}

/// Default idempotency directory when `--state-dir` and env are absent.
/// Under `$HOME/.bridge-withdraw-state/` so a system-wide install does not
/// clash across users.
/// `$HOME/.bridge-withdraw-state`, or a refusal.
///
/// **No cwd fallback.** Falling back to `.` makes the double-burn guard
/// relative to wherever the operator happened to be standing: the same
/// command run from two directories finds no prior record either time and
/// burns twice, silently. `HOME` is routinely unset under systemd, cron,
/// `sudo` without `-H`, and many Docker images — i.e. exactly the automated
/// contexts where nobody is watching.
///
/// Refusing costs one flag; guessing costs a second irreversible burn.
fn default_state_dir() -> CliResult<PathBuf> {
    let base = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| CliError::Preflight {
            reason: "HOME is not set, so there is no default --state-dir. That directory is the \
                     only thing stopping the same withdrawal from being burned twice, and a \
                     cwd-relative fallback would make it depend on where you were standing.\n\x20 \
                     Pass --state-dir (or set BRIDGE_WITHDRAW_STATE_DIR) to an absolute path that \
                     persists between runs."
                .to_string(),
            source: None,
        })?;
    Ok(PathBuf::from(base).join(".bridge-withdraw-state"))
}

/// Terminal confirmation before the AN burn. Prints the money-moving
/// details to stderr, reads a line from stdin, and refuses (Preflight
/// error, exit 2) unless the user types "y" or "yes" (case-insensitive).
///
/// Skipped when `--yes` is set. When stdin is closed / not a TTY, we
/// treat it as an implicit refusal — the operator should have passed
/// `--yes`, on its own or alongside `--non-interactive` (which without
/// `--yes` is already refused upstream in `main`).
fn confirm_before_burn(
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    args: &WithdrawArgs,
    anchor_mode: AnchorLayerMode,
) -> CliResult<()> {
    use std::io::{BufRead, IsTerminal, Write};

    let chain_name = args::SUPPORTED_CHAINS
        .iter()
        .find_map(|(id, name)| (*id == to.chain_id).then_some(*name))
        .unwrap_or("unknown");
    let wait_hint = match anchor_mode {
        AnchorLayerMode::Auto => "auto (~6-15 min on L1)",
        AnchorLayerMode::Explicit(1) => "~6-15 min (L1 stride)",
        AnchorLayerMode::Explicit(2) => "~91 min chain-time + prover (L2 stride)",
        AnchorLayerMode::Explicit(n) => {
            // For L≥3, no shipped relayer advances the anchor — user is on
            // their own. Say so out loud.
            let _ = n;
            "unbounded (no shipped relayer for L≥3)"
        },
    };

    // One string, one write, one result to check. It used to be sixteen
    // `let _ = writeln!` — and the answer this function then reads is
    // authoritative over an irreversible burn. With stderr unwritable
    // (EPIPE from a closed pager, ENOSPC, a closed fd) every discard
    // succeeded silently, the terminal sat there with no prompt on it,
    // and whatever the operator eventually typed was taken as consent to
    // the details they had not been shown.
    //
    // A redirect is NOT that case: `2>file` writes fine, and the prompt
    // is in the file. Only a write that actually fails gets refused.
    let prompt = format!(
        "\nAbout to withdraw USDC:\n\x20 from      : {from}\n\x20 to        : 0x{to_hex} on \
         {chain_name} (chain {chain_id})\n\x20 amount    : {amount_display} USDC\n\x20 bridge    \
         : {bridge}\n\x20 anchor    : {anchor_mode:?} (wait {wait_hint})\n\nThis will:\n\x20 1. \
         broadcast a multisig sendTransaction burning {amount_display} USDC on Acki Nacki\n\x20 \
         2. wait for the covering anchor bundle to land on Sepolia\n\x20 3. produce a Circuit-4 \
         SHPLONK proof\n\x20 4. submit withdrawByProof (spends ETH gas)\n\nThe AN burn is \
         irreversible once broadcast. Pass --yes to skip this prompt.\nProceed? [y/N]: ",
        from = from.extended(),
        to_hex = hex::encode(to.address.as_slice()),
        chain_id = to.chain_id,
        amount_display = amount.display(),
        bridge = args.bridge_address,
    );

    let mut stderr = std::io::stderr().lock();
    let shown = stderr
        .write_all(prompt.as_bytes())
        .and_then(|()| stderr.flush());
    drop(stderr);
    if let Err(e) = shown {
        return Err(CliError::Preflight {
            reason: format!(
                "could not print the burn confirmation to stderr ({e}) — refusing to accept an \
                 answer to a question you were never shown. Nothing was broadcast. Pass --yes if \
                 you meant to skip the prompt."
            ),
            source: None,
        });
    }

    if !std::io::stdin().is_terminal() {
        return Err(CliError::Preflight {
            reason: "stdin is not a TTY — pass --yes to skip confirmation, or run in a terminal"
                .to_string(),
            source: None,
        });
    }

    let mut line = String::new();
    let stdin = std::io::stdin();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::Preflight {
            reason: format!("failed to read confirmation from stdin: {e}"),
            source: None,
        })?;
    let answer = line.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        Err(CliError::Preflight {
            reason: format!(
                "confirmation declined (answered {:?}) — nothing broadcast",
                answer
            ),
            source: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reserved record, with `an_tx_hash` as the only variable.
    fn reserved(an_tx_hash: Option<&str>) -> idempotency::Record {
        idempotency::Record {
            key: "k".into(),
            status: Status::Reserved,
            from_extended: "ab::cd".into(),
            to_hex: "0x00".into(),
            to_chain: 11_155_111,
            amount_micro: 1_000_000,
            reserved_at: "1970-01-01T00:00:00Z".into(),
            an_tx_hash: an_tx_hash.map(str::to_string),
            withdrawal_msg_id: None,
            block_seq_no: None,
            eth_tx_hash: None,
        }
    }

    #[test]
    fn a_refusal_on_a_lockless_filesystem_declares_the_evidence_missing() {
        // The third liveness verdict, and until now the only one no test
        // reached: every call here passed `flock_available: true`, so the
        // arm that a state directory on NFS or an overlay mount takes was
        // production-only.
        //
        // What must NOT come out of it is the second verdict. "No other
        // process holds this withdrawal" is the sentence the runbook turns
        // into permission to delete the record, and on a mount where the
        // question cannot be asked, both racing runs would read it — one
        // of them possibly inside `burn::send`. Deleting there is the
        // second burn.
        let dir = tempfile::TempDir::new().unwrap();
        let err = decide_burn(
            &reserved(None),
            idempotency::Reservation::Found,
            dir.path(),
            None,
        )
        .expect_err("a hash-less record another run owns must still refuse");
        let msg = format!("{err}");
        assert!(
            msg.contains("could not be determined"),
            "the verdict has to say the question went unanswered: {msg}",
        );
        assert!(
            !msg.contains("No other process"),
            "the one sentence this arm may never produce — it is what authorises a deletion: {msg}",
        );
        // Same refusal, same remedy: only the evidence line differs.
        assert_eq!(err.exit_code().as_i32(), 3, "{err:?}");
        assert!(
            msg.contains("Reconcile on chain"),
            "with no lock evidence the on-chain check is all there is, so it must still be named: \
             {msg}",
        );
    }

    #[test]
    fn a_record_we_did_not_create_and_that_has_no_hash_refuses() {
        // The concurrent half. Two runs, same identity, both --allow-retry,
        // no prior record: both `peek` → None, A wins the publish and
        // enters the multi-second `burn::send`, B gets EEXIST and reads A's
        // record — `Reserved`, `an_tx_hash` still None, because A has not
        // returned yet. Deciding on the fields alone says Send, and that is
        // a second initiateWithdrawal.
        //
        // The same shape covers a hand-edited `{"status":"burned",
        // "an_tx_hash":null}`, which the CLI's own post-burn recovery
        // message invites an operator to produce.
        let dir = tempfile::TempDir::new().unwrap();
        let err = decide_burn(
            &reserved(None),
            idempotency::Reservation::Found,
            dir.path(),
            Some(false),
        )
        .expect_err("a record another run owns, with no hash yet, must refuse");
        assert!(
            matches!(err, CliError::ReservationInFlight { .. }),
            "the honest answer is exit 3, not a second burn: {err:?}",
        );
        // And the refusal has to say what to do, or the only escape an
        // operator finds is deleting the guard.
        let msg = format!("{err}");
        assert!(
            msg.contains("--allow-retry does NOT override"),
            "must not send them back to the flag they already passed: {msg}"
        );
        assert!(msg.contains("Record:"), "must name the file: {msg}");
        assert!(
            msg.contains("Reconcile on chain"),
            "must give the first step: {msg}"
        );
        assert!(
            msg.contains("while another run is mid-send"),
            "must state the precondition on deleting it: {msg}"
        );
    }

    #[test]
    fn a_reservation_that_already_names_a_burn_must_not_send_again() {
        // The `--allow-retry` race. `peek` runs in stage 1; `reserve` runs
        // after the EVM preflight, the confirmation prompt and `compose` —
        // a window that is unbounded when the prompt is live. Another run
        // on this host can reserve, burn and record its hash inside it, and
        // `reserve` with `--allow-retry` then hands that record back.
        //
        // Deciding from `peek` sends a second `initiateWithdrawal` against a
        // multisig with no replay guard. Deciding from what `reserve`
        // returned does not.
        assert_eq!(
            decide_burn(
                &reserved(Some("0xdead")),
                idempotency::Reservation::Found,
                Path::new("/nonexistent"),
                Some(false),
            )
            .unwrap(),
            BurnDecision::Reuse("0xdead".into()),
            "a record naming a broadcast burn must be resumed, never re-sent",
        );
    }

    #[test]
    fn a_fresh_reservation_sends() {
        assert_eq!(
            decide_burn(
                &reserved(None),
                idempotency::Reservation::Created,
                Path::new("/nonexistent"),
                Some(false),
            )
            .unwrap(),
            BurnDecision::Send,
            "no hash on the record means nothing reached the wire for this identity",
        );
    }

    // -- The seam: reserve + decide, against a real state directory ------
    //
    // `decide_burn` is right on its own and `reserve` is right on its own.
    // The bug 4660006 fixed was in neither: the caller decided from the
    // stage-1 `peek`. These drive the composition, which is the only place
    // that class of defect is visible.

    fn seam_from() -> FromAddress {
        FromAddress {
            dapp_id_hex: "a".repeat(64),
            account_id_hex: "b".repeat(64),
        }
    }

    fn seam_to() -> ToAddress {
        ToAddress {
            address: "0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f"
                .parse()
                .unwrap(),
            chain_id: 11_155_111,
        }
    }

    #[test]
    fn the_first_run_on_a_fresh_identity_sends() {
        let dir = tempfile::TempDir::new().unwrap();
        let (r, d, _lock) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .unwrap();
        assert_eq!(d, BurnDecision::Send);
        assert_eq!(r.status, Status::Reserved);
        assert!(r.an_tx_hash.is_none());
    }

    #[test]
    fn a_second_run_while_the_first_is_still_sending_refuses() {
        // The exact production sequence, in order: run A reserves and is
        // now inside `burn::send` — its record exists, `Reserved`, with no
        // hash, because the hash is written only after the send returns.
        // Run B arrives with --allow-retry. Nothing in the record says
        // "someone is mid-send"; only the reservation's provenance does.
        let dir = tempfile::TempDir::new().unwrap();
        let (_a, da, _lock_a) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .unwrap();
        assert_eq!(da, BurnDecision::Send, "A broadcasts");

        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .expect_err("B must not broadcast a second initiateWithdrawal");
        assert!(
            matches!(err, CliError::ReservationInFlight { .. }),
            "exit 3, not a second burn: {err:?}"
        );
        // A's lock is still held — that IS the premise of this test — so
        // the refusal can say so outright instead of leaving B to guess.
        let msg = format!("{err}");
        assert!(
            msg.contains("RIGHT NOW"),
            "must name the live holder: {msg}"
        );
        assert!(
            msg.contains("do not delete it"),
            "the one thing that would cause the second burn: {msg}"
        );

        // Now A exits. The record is unchanged and still says nothing
        // about whether a burn landed — but the question "is anyone
        // executing this" now has a different, checkable answer, and the
        // refusal has to change with it.
        drop(_lock_a);
        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .expect_err("a dead run's record still cannot be sent over");
        let msg = format!("{err}");
        assert!(
            msg.contains("has already exited"),
            "must say the holder is gone: {msg}"
        );
        assert!(
            msg.contains("not the same as"),
            "and must not let that be read as 'nothing was broadcast': {msg}"
        );
    }

    #[test]
    fn a_lock_this_run_could_not_take_refuses_instead_of_continuing_unlocked() {
        // The blanket downgrade. Every failure to take the lock used to
        // read as "this filesystem has no flock" and the run carried on
        // holding nothing — which is invisible to the liveness probe, so
        // the next run is told this one "has already exited" and the
        // runbook makes that the condition for deleting the record. The
        // failures that reach this are the asymmetric ones: `EMFILE` is
        // per-process, so the other run opens the same lock normally.
        //
        // A directory where the lock file goes is the uid-independent way
        // to make the open fail while leaving the state directory itself
        // perfectly usable — unlike a chmod, which root ignores.
        let dir = tempfile::TempDir::new().unwrap();
        let k = idempotency::key(&seam_from(), &seam_to(), &UsdcAmount(1_000_000));
        std::fs::create_dir(dir.path().join(format!("{k}.lock"))).unwrap();

        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a lock this run cannot take is not a lock nobody needs");
        assert_eq!(err.exit_code().as_i32(), 2, "nothing was sent: {err}");
        let msg = format!("{err}");
        assert!(
            msg.contains("nothing was sent"),
            "the refusal must say so outright: {msg}",
        );

        // And it refused BEFORE reserving, so it left no record for a
        // later run to be told is safe to delete.
        let records: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert!(records.is_empty(), "refused before reserving: {records:?}");
    }

    #[test]
    fn a_state_directory_that_cannot_be_prepared_is_not_a_verdict_about_locking() {
        // Two questions, and the first must not answer the second. A
        // failed `mkdir` used to arrive at the caller as `Err` from
        // `try_acquire`, indistinguishable from `flock` being unavailable,
        // and was read as the latter.
        let dir = tempfile::TempDir::new().unwrap();
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();

        let err = reserve_and_decide(
            &blocker.join("state"),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a state directory under a regular file cannot be created");
        assert_eq!(err.exit_code().as_i32(), 2, "{err}");
        let msg = format!("{err}");
        assert!(
            msg.contains("mkdir"),
            "and it says the directory is the problem, not the lock: {msg}",
        );
    }

    #[test]
    fn the_first_run_in_a_fresh_state_dir_still_holds_the_lock() {
        // The lock is taken before the reservation, and the reservation is
        // what creates the state directory — so on a first invocation the
        // lock's own `open` hit ENOENT. That is reported as "flock could
        // not be attempted", which is a supported deployment (a network
        // mount) and therefore not a refusal: the run proceeds holding
        // nothing.
        //
        // What that costs is the liveness verdict, and the verdict is what
        // authorises deleting a record. A concurrent retry probes a lock
        // nobody holds, is told the first run "has already exited", and
        // the documented recovery then invites deleting the record while
        // the first run is inside `burn::send`.
        let dir = tempfile::TempDir::new().unwrap();
        let fresh = dir.path().join("state-dir-that-does-not-exist-yet");
        assert!(!fresh.exists());

        let (_r, d, lock) = reserve_and_decide(
            &fresh,
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect("a first withdrawal in a fresh state dir is ordinary");
        assert_eq!(d, BurnDecision::Send);
        assert!(
            lock.is_some(),
            "the very first run must own its withdrawal too, or nothing can tell a later run that \
             it is still alive",
        );

        // And that is exactly what a concurrent retry asks.
        let k = idempotency::key(&seam_from(), &seam_to(), &UsdcAmount(1_000_000));
        assert_eq!(
            idempotency::WithdrawalLock::probe_holder(&fresh, &k),
            Some(true),
            "a retry must see the first run holding this withdrawal, not a free lock",
        );
    }

    #[test]
    fn a_plain_retry_over_a_dead_runs_reservation_gets_the_actionable_refusal() {
        // The dead end, in the one arrangement that still reached it. A
        // run peeks and sees nothing; another run reserves and dies during
        // this one's preflight; this one then reserves without
        // `--allow-retry`.
        //
        // That used to earn "Reconcile via GraphQL, or re-run with
        // --allow-retry" — and re-running with the flag lands on a refusal
        // whose text is "--allow-retry does NOT override this". Two
        // refusals, the first sending the operator to the second, and the
        // only escape either of them left to find was deleting the record,
        // which is what permits a second burn.
        let dir = tempfile::TempDir::new().unwrap();
        let (r, d, lock_a) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .unwrap();
        assert_eq!(d, BurnDecision::Send);
        assert!(r.an_tx_hash.is_none(), "A died before writing a hash");
        drop(lock_a); // A exits, however it exits — the kernel frees it.

        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a hash-less record somebody else published is not this run's to burn");
        assert_eq!(err.exit_code().as_i32(), 3, "{err:?}");

        let msg = format!("{err}");
        assert!(
            msg.contains("has already exited"),
            "the liveness verdict, which is what says deleting is even discussable: {msg}",
        );
        assert!(msg.contains("Record:"), "and the path to delete: {msg}");
        assert!(
            msg.contains("--allow-retry does NOT override"),
            "the flag must not be offered by a refusal that then refuses it: {msg}",
        );
        assert!(
            !msg.contains("re-run with --allow-retry to resume"),
            "that advice belongs to records carrying a hash, not this one: {msg}",
        );
    }

    #[test]
    fn a_resumed_run_reuses_the_recorded_burn() {
        // Once A's hash IS on the record, B resumes at capture instead of
        // burning again — the same refusal would strand a recoverable
        // withdrawal.
        let dir = tempfile::TempDir::new().unwrap();
        let (mut r, _, _lock) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .unwrap();
        r.status = Status::Burned;
        r.an_tx_hash = Some(format!("0x{}", "ab".repeat(32)));
        idempotency::update(dir.path(), &r).unwrap();
        // A exits: the resume is a LATER run, not a concurrent one. Without
        // this the lock refuses first and the test would be asserting the
        // wrong thing.
        drop(_lock);

        let (_, d, _lock) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .unwrap();
        assert_eq!(d, BurnDecision::Reuse(format!("0x{}", "ab".repeat(32))));
    }

    // -- The resume path ---------------------------------------------------
    //
    // It used to call `reserve` directly and throw the answer away. These
    // drive the seam production now goes through — the previous round's
    // tests exercised `reserve_and_decide`, which the resume branch never
    // reached, so the defect lived in the one place the coverage did not.

    fn burned_record(dir: &Path, hash: &str) -> idempotency::Record {
        let (mut r, _, lock) =
            reserve_and_decide(dir, &seam_from(), &seam_to(), &UsdcAmount(1_000_000), false)
                .unwrap();
        r.status = Status::Burned;
        r.an_tx_hash = Some(hash.to_string());
        idempotency::update(dir, &r).unwrap();
        drop(lock);
        r
    }

    #[test]
    fn everything_that_can_refuse_runs_before_the_reservation() {
        // The order inside `run`'s burn branch IS the fix, and nothing but
        // the order enforces it. The prompt comes first (a human can still
        // say no), then `compose` (a bad keys.json, an unresolvable
        // USDCBridge, a preflight report for a different withdrawal), and
        // only then the reservation — the first thing this command leaves
        // on disk. Reserving earlier compiles, passes every other test,
        // and turns a declined prompt into a record that refuses the
        // identical re-run with exit 3 for a burn that never happened.
        //
        // Read `run`'s body alone: the resume path and the tests below
        // call `reserve_and_decide` too, with an ordering of their own.
        let src = include_str!("orchestrator.rs");
        let from = src
            .find(concat!("pub async fn ", "run("))
            .expect("run() is this module's entry point");
        let body = &src[from..];
        // Up to the next top-level item. Everything inside a function is
        // indented, so a column-0 `fn` is where this one ends.
        let body = &body[..body.find("\nfn ").unwrap_or(body.len())];
        // The comments in there discuss these calls by name; only the
        // code decides what runs when.
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let at = |what: &str| {
            code.find(what).unwrap_or_else(|| {
                panic!("run() no longer calls {what} — the ordering cannot be verified")
            })
        };
        let confirm = at(concat!("confirm_before", "_burn("));
        let compose = at(concat!("burn::com", "pose("));
        let reserve = at(concat!("reserve_and", "_decide("));
        let send = at(concat!("burn::s", "end("));

        assert!(
            confirm < compose,
            "the prompt runs before composing, so a declined burn composes nothing and holds no \
             owner key",
        );
        assert!(
            compose < reserve,
            "composing runs before reserving: it is the last step that can fail, and failing \
             after the reservation leaves a record that refuses the identical re-run",
        );
        assert!(
            reserve < send,
            "reserving runs before sending: the record is what makes a second burn refusable, and \
             after the send it is too late to write one",
        );
    }

    #[test]
    fn the_resume_hands_its_lock_to_a_binding_that_outlives_the_burn() {
        // The other half of the same one-character family. Making the
        // lock an out-parameter removed the tuple slot a `_` could
        // swallow, but the call site can still pass a temporary: `&mut
        // None` compiles, satisfies the type, keeps clippy quiet, and
        // drops the guard at the end of the statement. The run then walks
        // into `burn::send` holding nothing — invisible to the liveness
        // probe every recovery procedure in the runbook leans on, and the
        // exact state the lock was added to eliminate.
        //
        // `a_resume_takes_the_withdrawal_lock` proves the seam fills the
        // slot it is handed. Nothing but this proves `run` hands it one
        // that lives.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let lines: Vec<&str> = production.lines().collect();

        let calls: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                t.contains(concat!("resume_recorded", "_burn("))
                    && !t.starts_with("//")
                    && !t.starts_with("fn ")
            })
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "one call site, so one place to check; a second one needs its own argument checked \
             here rather than inheriting this one's clean bill: {calls:?}",
        );

        // The argument list, not the whole function: a `_withdrawal_lock`
        // mentioned anywhere else in the file must not vouch for this.
        let start = calls[0];
        let end = (start..lines.len())
            .find(|&n| lines[n].contains(")?;"))
            .map(|n| n + 1)
            .unwrap_or(lines.len());
        let args = lines[start..end].join("\n");
        assert!(
            args.contains(concat!("&mut _withdrawal", "_lock")),
            "the resume must be handed the binding that lives to the end of `run`, not a \
             temporary that releases the lock as the statement ends: {args}",
        );

        // The OTHER path out of `run`, which still takes the lock from a
        // tuple. `let (r, decision, _)` there compiles, passes clippy and
        // leaves the whole suite green — measured, not assumed — because
        // no test drives `run`'s burn branch: it needs a chain.
        //
        // `BurnPermit::issue` catches it at runtime and refuses before
        // broadcasting, which is the defence that matters. This is the
        // one that fails in CI instead of on somebody's withdrawal. A
        // syntactic property of one line is what grep is actually good
        // for; what it must not be asked is whether the lock is still
        // held ninety lines later, which is why the permit exists.
        let reserves: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                // BOTH names. `reserve_and_decide_holding(` does not
                // contain `reserve_and_decide(` — the trailing paren made
                // the helper invisible to this scan, and it returns the
                // same three-element tuple.
                (t.contains(concat!("reserve_and", "_decide("))
                    || t.contains(concat!("reserve_and_decide", "_holding(")))
                    && !t.starts_with("//")
                    && !t.starts_with("///")
                    // The definition, not a call.
                    && !t.starts_with("fn ")
            })
            .map(|(n, _)| n)
            .collect();
        // Two: `run`'s burn branch and the resume seam. The seam's is
        // inside `reserve_and_decide_holding`, whose lock goes on to the
        // out-parameter checked above.
        for n in reserves {
            // The WHOLE statement, found by walking back to the previous
            // one, not a fixed two-line window. A window cannot tell a
            // wrapped destructuring from a tail call that happens to sit
            // under an unrelated `let`, and judging tail-ness by "does
            // this text contain `let (`" exempted every non-tuple
            // binding — `let x = reserve_and_decide(..)?;` places no lock
            // at all and passed.
            let mut start = n;
            while start > 0 {
                let prev = lines[start - 1].trim_end();
                if prev.ends_with(';') || prev.ends_with('{') || prev.ends_with('}') {
                    break;
                }
                start -= 1;
            }
            let stmt = lines[start..=n].join(" ");
            // Not just `, _)`. A tuple pattern has other ways to let go
            // of the third element and all of them compile: `..` swallows
            // it wholesale, and a `_`-prefixed BINDING lives only to the
            // end of its statement unless the caller places it somewhere.
            // The first version of this check knew one of them.
            for evasion in [", _)", ", _ )", ", ..)", ", _lock)", ", _l)"] {
                assert!(
                    !stmt.contains(evasion),
                    "the lock is the third element and `{evasion}` lets go of it: \
                     orchestrator.rs:{}: {stmt}",
                    n + 1,
                );
            }
            // And the positive form, so a spelling nobody anticipated has
            // to be named here rather than quietly discarded there. A
            // tail-position call returns the tuple onward and destructures
            // nothing, which is the one shape with no slot to check.
            // A tail expression BINDS NOTHING — it hands the tuple, lock
            // included, to its own caller. Anything that binds must name
            // the lock. Keyed on `let ` rather than `let (`, because
            // `let whatever = reserve_and_decide(..)` binds too, and
            // exempting it left the third element to be dropped at the
            // end of the statement with the suite green.
            let is_tail_call = !stmt.contains("let ");
            assert!(
                is_tail_call || stmt.contains(", lock)"),
                "the lock has to land in a named binding the caller then places: \
                 orchestrator.rs:{}: {stmt}",
                n + 1,
            );
            // Binding it and dropping it on the next line is the same
            // release with two statements instead of one. Look at what
            // happens to `lock` before the enclosing block ends.
            if !is_tail_call {
                let after = lines[n..(n + 6).min(lines.len())].join("\n");
                assert!(
                    !after.contains(concat!("drop(", "lock)")),
                    "the lock is bound and then dropped, which is the underscore with extra \
                     steps: orchestrator.rs:{}: {after}",
                    n + 1,
                );
            }
        }
    }

    #[test]
    fn the_resume_branch_never_reads_the_peeked_hash_after_reserving() {
        // The defect a commit named after it did not close. The seam
        // returns the hash the RESERVATION read; a test pins the seam;
        // nothing pinned that `run` uses what came back. Going back to
        // the peeked hash is one underscore of edit — `let (r, _an_tx,
        // lock)` and then read `p.an_tx_hash` — and it leaves the suite
        // and `clippy -D warnings` both green while capture waits out its
        // timeout against a transaction nobody is looking for, with the
        // burn already on the wire.
        //
        // What is asserted is the invariant, not the spelling: after the
        // reservation, the record stage 1 peeked is not read for its hash
        // again. The evasion above REQUIRES that read — underscore the
        // binding and there is nothing left to return — so this catches
        // it whatever the binding is called.
        //
        // Its limit, stated rather than papered over: copying the peeked
        // hash into a local BEFORE the call and using that afterwards is
        // invisible to any text check. That is a deliberate act, not a
        // one-character slip, which is the difference this is drawn at.
        let src = include_str!("orchestrator.rs");
        let from = src
            .find(concat!("pub async fn ", "run("))
            .expect("run() is this module's entry point");
        let body = &src[from..];
        let body = &body[..body.find("\nfn ").unwrap_or(body.len())];

        let branch_start = body
            .find(concat!("if let Some(p) = prior.as_ref()", ".filter("))
            .expect("run() no longer has a resume branch — this guard cannot be verified");
        let branch = &body[branch_start..];
        let branch = &branch[..branch
            .find("\n        } else {")
            .expect("the resume branch is the `if` half of the burn/resume choice")];

        let call = branch
            .find(concat!("resume_recorded", "_burn("))
            .expect("the resume branch no longer goes through the seam");

        let after_the_reservation = &branch[call..];
        let offenders: Vec<_> = after_the_reservation
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(concat!("p.an_tx", "_hash")))
            .map(|l| l.trim().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "the peek is minutes older than the reservation, and the exit-3 remedy tells \
             operators to rewrite the hash in exactly that window — so once the reservation has \
             answered, its record is the only one this run may act on: {offenders:?}",
        );
    }

    #[test]
    fn no_state_write_in_this_pipeline_claims_that_nothing_was_sent() {
        // `UpdateFailed` takes away the `?` and forces a choice at every
        // call site. The choice can still be made wrongly: the other
        // constructor compiles at any of them and renders a burn that is
        // already on the wire as exit 2 — "refused before sending,
        // nothing left the machine" — which is the single sentence the
        // type exists to make unsayable, and the one that gets a second
        // burn fired by a retry wrapper.
        //
        // Every `idempotency::update` in this file is downstream of the
        // burn: the seven of them run at stages 3 through 6. So the
        // pre-send constructor has no business here at all. If a write
        // that genuinely precedes the send is ever added, this guard is
        // where that gets recorded — deliberately, and read by a
        // reviewer, rather than slipping in as one more call site.
        // Watching the constructor alone was not enough, and the gap was
        // not hypothetical: `resume_recorded_burn` grew a hand-built
        // `CliError::Preflight` whose text ended "Nothing was sent." on a
        // path entered only when the peeked record carries an
        // `an_tx_hash`. It never touched `before_send`, so this guard
        // stayed green over the exact sentence it was written to forbid.
        //
        // So watch the CLAIM as well as the constructor. The sentence is
        // the thing that costs money — a retry wrapper reads it and fires
        // a second burn — and it does not become safe by being spelled out
        // by hand instead of reached through the type.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let claim = concat!("nothing was ", "sent");
        let offenders: Vec<_> = production
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| {
                l.contains(concat!("before", "_send(")) || l.to_ascii_lowercase().contains(claim)
            })
            .map(|(n, l)| format!("orchestrator.rs:{}: {}", n + 1, l.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "every refusal in this pipeline is downstream of the burn, so none of them may say \
             that nothing was sent — not through `before_send`, and not in prose: {offenders:?}",
        );
    }

    #[test]
    fn every_place_the_lock_is_installed_takes_a_hold_of_it() {
        // The holds ARE the protection, and nothing referenced them.
        // Deleting one or two ships green — silently reverting to round
        // 8's coverage, or round 9's — because the borrow they open is
        // the only thing that fails, and it fails only where somebody
        // later releases the lock. Deleting all three is caught by
        // dead_code, which is the harmless edit; the damaging one was
        // invisible.
        //
        // Three sites, one per place the lock becomes this run's:
        //   * after the resume seam fills the out-parameter,
        //   * after the burn branch's reservation, before the `match` so the `Reuse`
        //     arm is covered too,
        //   * once the branch has closed, above the dry-run return.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let lines: Vec<&str> = production.lines().collect();

        let holds: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                t.contains(concat!("WithdrawalLock::", "hold")) && !t.starts_with("//")
            })
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            holds.len(),
            3,
            "one hold per place the lock is installed; fewer means a stretch of the run is back \
             on convention, and the compiler will not say which: {holds:?}",
        );

        // And each one is where it is for a reason, so a hold that
        // survives the count while moving somewhere useless still fails.
        let installs = [
            (concat!("&mut _withdrawal", "_lock,"), "the resume seam"),
            (
                concat!("_withdrawal_lock = ", "lock;"),
                "the burn branch's reservation",
            ),
            (
                concat!("let _withdrawal_lock = _withdrawal", "_lock;"),
                "the settled rebinding",
            ),
        ];
        for (anchor, what) in installs {
            let at = lines
                .iter()
                .position(|l| l.contains(anchor))
                .unwrap_or_else(|| panic!("{what} is gone: {anchor}"));
            assert!(
                // Twelve, not three: each hold carries the paragraph
                // explaining why it is there, and rustfmt wraps the
                // `.as_ref().map(..)` across three lines of its own.
                holds.iter().any(|&h| h > at && h - at <= 12),
                "{what} installs the lock and nothing takes a hold of it within twelve lines, so \
                 everything after it is unprotected",
            );
        }
    }

    #[test]
    fn the_hash_less_refusal_is_raised_from_exactly_three_places() {
        // `errors.rs` describes where this refusal comes from, and an
        // operator reading that description is being told which
        // situations produce it. It said "two places" while there were
        // three — the contended-lock arm was added and the prose was not
        // — so one of the three situations had no documented existence.
        //
        // A count rather than a list of line numbers: line numbers rot
        // every commit, and what a reader needs is to be sent back here
        // when the set changes at all.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let sites: Vec<&str> = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(concat!("CliError::", "ReservationInFlight")))
            .collect();
        assert_eq!(
            sites.len(),
            3,
            "the hash-less refusal is raised from three places — stage 1, the contended lock, and \
             `decide_burn`. If that changed, `errors.rs`'s description of where it comes from has \
             to change with it: {sites:#?}",
        );
    }

    #[test]
    fn a_stage_one_refusal_names_the_burn_it_is_refusing_around() {
        // The nine-round thread, decided the way the project had already
        // decided it once: entry to the resume arm is "gated on a burn
        // being on the wire" and its refusals are exit 10, and
        // `burn_already_sent` is the same predicate one stage earlier.
        //
        // Exit 2's contract has two halves. "Nothing new was broadcast or
        // written" survives a stage-1 refusal. "There is no state file and
        // no burn" does not, and that is the half the runbook turns into
        // "no state file was written" — while the exit-10 page says "do
        // not delete the state file" about the very same run.
        let an = format!("0x{}", "1d".repeat(32));
        let refused = CliError::Preflight {
            reason: "multisig has 2 custodians, expected 1".into(),
            source: None,
        };

        // With no recorded burn, exit 2 is the truth and nothing moves.
        let untouched = refusal_before_a_recorded_burn(
            CliError::Preflight {
                reason: "multisig has 2 custodians, expected 1".into(),
                source: None,
            },
            None,
        );
        assert_eq!(
            untouched.exit_code().as_i32(),
            2,
            "a first run's preflight refusal is exactly what exit 2 is for: {untouched}",
        );

        let rebadged = refusal_before_a_recorded_burn(refused, Some(&an));
        assert_eq!(
            rebadged.exit_code().as_i32(),
            10,
            "with a burn on the wire, exit 2 tells a retry wrapper the state is clean: {rebadged}",
        );
        let msg = format!("{rebadged}");
        assert!(
            msg.contains(&an) && msg.contains("multisig has 2 custodians"),
            "the operator needs both the reason they were refused and the transaction they must \
             not act around: {msg}",
        );
        assert!(
            msg.contains("Do not delete that record"),
            "which is the sentence the exit-2 page contradicts: {msg}",
        );

        // Exit 3 and everything past the send pass through: they carry
        // remedies of their own and re-badging them would hide those.
        let duplicate = refusal_before_a_recorded_burn(
            CliError::CaptureTimeout {
                an_tx: an.clone(),
            },
            Some(&an),
        );
        assert_eq!(
            duplicate.exit_code().as_i32(),
            11,
            "only the exit-2 half moves: {duplicate}",
        );
    }

    #[test]
    fn no_stage_one_refusal_hides_a_recorded_burn() {
        // The class, not the six sites. Stage 1 is a run of `preflight::`
        // calls, every one of which speaks exit 2, and each was fixed one
        // review round at a time for nine rounds. A seventh added below
        // this line inherits the same defect, so the region is checked
        // rather than the list.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let from = production
            .find(concat!("let burn_already", "_sent ="))
            .expect("stage 1 begins where the predicate is computed");
        let to = production[from..]
            .find(concat!("burn::", "compose("))
            .map_or(production.len(), |i| from + i);
        let region: Vec<&str> = production[from..to].lines().collect();

        let mut bare = Vec::new();
        for (n, line) in region.iter().enumerate() {
            if line.trim_start().starts_with("//") || !line.contains("preflight::") {
                continue;
            }
            // To the end of this statement, at depth zero.
            let mut depth = 0i32;
            let mut end = region.len();
            for (k, l) in region.iter().enumerate().skip(n) {
                depth += l.chars().filter(|&c| c == '(' || c == '{').count() as i32;
                depth -= l.chars().filter(|&c| c == ')' || c == '}').count() as i32;
                if depth <= 0 && l.trim_end().ends_with(';') {
                    end = k + 1;
                    break;
                }
            }
            let stmt = region[n..end].join("\n");
            // A `let` of a type, or a call that never fails, has no `?`.
            if stmt.contains('?') && !stmt.contains(concat!("refusal_before_a_recorded", "_burn")) {
                bare.push(stmt);
            }
        }
        assert!(
            bare.is_empty(),
            "a stage-1 refusal propagated bare reports exit 2 — \"nothing broadcast, no state \
             file written\" — about a withdrawal whose burn may already be on the wire and whose \
             record is on disk: {bare:#?}",
        );
    }

    #[test]
    fn no_refusal_after_the_send_claims_to_be_pre_send() {
        // The class, rather than the two spellings the guard above knows.
        // A `CliError::Preflight` IS the claim — exit 2's published
        // contract is "nothing broadcast, no state file written" — and it
        // makes that claim whether or not the words appear in its text.
        // One had been sitting at stage 4 since before any of these
        // rounds: `split_extended` on a string preflight had already
        // parsed, rendered as exit 2 with the burn on the wire.
        //
        // Textual position stands in for pipeline position, and does so
        // honestly here: `run` is written in stage order, every path that
        // reaches stage 4 has passed the send or resumed a recorded one,
        // and the resume arm sits above this line rather than below it.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let run_at = production
            .find(concat!("pub async fn ", "run("))
            .expect("run() is this module's entry point");
        let body = &production[run_at..];
        // Up to the next top-level item, the same cut the ordering guard
        // uses: everything inside a function is indented.
        let end = body
            .match_indices("\nfn ")
            .next()
            .map_or(body.len(), |(i, _)| i);
        let after_send = body[..end]
            .find(concat!("burn::", "send("))
            .expect("the burn branch calls the send by name");

        // ALL FOUR variants that map to `ExitCode::PreflightRefused`, not
        // just the one this guard was written around. `parse_eth_signer`
        // returns `ArgInvalid`, which is the same exit 2 by a different
        // name, and the first version of this guard could not see it.
        let post_send = &body[after_send..end];
        let mut offenders: Vec<String> = post_send
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| {
                [
                    concat!("CliError::", "Preflight"),
                    concat!("CliError::", "ArgInvalid"),
                    concat!("CliError::", "KeyFilePerms"),
                    concat!("CliError::", "Usage"),
                ]
                .iter()
                .any(|v| l.contains(v))
            })
            .map(|l| l.trim().to_string())
            .collect();

        // And the hole a variant grep cannot close: a call into
        // `preflight`, whose whole vocabulary is pre-send, propagated with
        // a bare `?`. No grep can see what a function returns, so the set
        // of such calls is pinned instead — adding one means coming here
        // and recording what its error maps to.
        //
        //   `recheck_proving_key` → `ProofFailed`, exit 12. Correct on its
        //   own: it runs at stage 5 and its message says the burn is on
        //   the wire. Propagating it bare is fine.
        //   `parse_eth_signer` → `ArgInvalid`, which is exit 2 by another
        //   name. It MUST be re-badged at the call site, so the entry
        //   below carries that requirement and the check enforces it —
        //   an allowlist that only asked "is this call known?" let the
        //   `map_err` be deleted again with everything green.
        let known: &[(&str, bool)] = &[("recheck_proving_key", false), ("parse_eth_signer", true)];
        let post_send_lines: Vec<&str> = post_send.lines().collect();
        for (n, line) in post_send_lines.iter().enumerate() {
            if line.trim_start().starts_with("//") || !line.contains("preflight::") {
                continue;
            }
            let Some((name, needs_rebadge)) = known.iter().find(|(k, _)| line.contains(k)) else {
                offenders.push(format!(
                    "unreviewed preflight:: call past the send — record what its error maps to: {}",
                    line.trim(),
                ));
                continue;
            };
            if !needs_rebadge {
                continue;
            }
            // The whole statement, which rustfmt may have wrapped: from
            // this line to the one that ends it.
            // The END OF THIS STATEMENT, not the next `?;` anywhere
            // below. A bare `?` inline in a multi-line expression has no
            // `?;` of its own, so the old scan ran on — measured at 48
            // lines past the call — until it met an unrelated `map_err`
            // and pronounced the site clean.
            //
            // A statement ends at the first line whose trailing character
            // is `;` at this nesting depth. Tracking depth is what stops
            // a `;` inside a nested closure from ending it early.
            let mut depth = 0i32;
            let mut end_of_stmt = post_send_lines.len();
            for (k, l) in post_send_lines.iter().enumerate().skip(n) {
                depth += l.chars().filter(|&c| c == '(' || c == '{').count() as i32;
                depth -= l.chars().filter(|&c| c == ')' || c == '}').count() as i32;
                if depth <= 0 && l.trim_end().ends_with(';') {
                    end_of_stmt = k + 1;
                    break;
                }
            }
            let stmt = post_send_lines[n..end_of_stmt].join("\n");
            if !stmt.contains("map_err") {
                offenders.push(format!(
                    "`{name}` returns a pre-send error and is propagated bare past the send: \
                     {stmt}",
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "past the send the burn is on the wire, so exit 2 — whose contract is that nothing \
             was broadcast and no state file was written — is a lie whatever the reason says. \
             Pick the code for the STAGE: {offenders:?}",
        );
    }

    #[test]
    fn the_duplicate_refusal_is_never_built_by_hand_in_this_file() {
        // What this checks and what it used to CLAIM to check are not the
        // same thing, and the gap was already load-bearing.
        //
        // It was called "no refusal in this file is the one without a
        // liveness verdict" and said every refusal here is about a
        // hash-less record. That stopped being true when the resume path
        // grew a terminal case: `resume_recorded_burn` restores a deleted
        // `confirmed` record and then refuses it, which IS a
        // `DuplicateInFlight` and is correct — the record carries a hash,
        // so the liveness verdict is not the half that is missing. The
        // assertion stayed green only because the constructor moved into
        // `idempotency::terminal_refusal` one module over. A guard whose
        // stated invariant is false and whose text still passes is worse
        // than none: it reads as coverage.
        //
        // The rule that survives, narrower and true: this file does not
        // BUILD that variant. Its remedy differs per status — the wrong
        // half of one shared sentence sent operators to a flag that
        // refuses them — so it has exactly one owner. The mapping itself
        // is pinned by
        // `a_refusal_about_a_hash_less_record_carries_the_liveness_verdict`
        // below, behaviourally, which is where an invariant of that shape
        // belongs.
        //
        // Production code only. The test below names the variant in order
        // to assert which refusal comes back, and a guard its own
        // neighbours trip over gets loosened until it means nothing.
        let src = include_str!("orchestrator.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
        let wrong = concat!("CliError::Duplicate", "InFlight {");
        let offenders: Vec<_> = production
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| l.contains(wrong))
            .map(|(n, l)| format!("orchestrator.rs:{}: {}", n + 1, l.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "build it through `idempotency::terminal_refusal`, which owns the per-status remedy; \
             a second construction site is how the two came to disagree: {offenders:?}",
        );
    }

    #[test]
    fn a_refusal_about_a_hash_less_record_carries_the_liveness_verdict() {
        // The invariant the guard above was named after, asserted where
        // it can actually be checked: which variant an exit 3 uses is
        // decided by whether the prior record carries an AN tx hash, and
        // by nothing else — not by which stage noticed, not by the flags.
        //
        // A hash-less record cannot say whether a burn is on the wire, so
        // its refusal must carry what the lock can say instead. A record
        // with a hash can, so its refusal talks about resuming.
        let dir = tempfile::TempDir::new().unwrap();

        // 1. Hash-less, left by a run that has exited.
        let (_r, _d, lock) = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .unwrap();
        drop(lock);
        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a hash-less record somebody else published");
        assert!(
            matches!(err, CliError::ReservationInFlight { .. }),
            "no hash means the verdict is the missing half: {err:?}",
        );
        assert!(format!("{err}").contains("has already exited"));

        // 2. The same identity once it carries a hash, without the flag.
        let mut r = idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
            .unwrap()
            .unwrap();
        r.status = Status::Burned;
        r.an_tx_hash = Some(format!("0x{}", "5c".repeat(32)));
        idempotency::update(dir.path(), &r).unwrap();

        let err = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a recorded burn without --allow-retry");
        assert!(
            matches!(err, CliError::DuplicateInFlight { .. }),
            "a hash makes the flag the answer, and that is the other variant: {err:?}",
        );
        let msg = format!("{err}");
        assert!(msg.contains("--allow-retry"), "{msg}");
        assert!(
            !msg.contains("does NOT override"),
            "that sentence belongs to the hash-less refusal: {msg}",
        );
    }

    #[test]
    fn a_resume_takes_the_withdrawal_lock() {
        // (a) and (b) of the defect: without this a resuming run was
        // invisible to the liveness probe the exit-3 refusal promises, and
        // two concurrent resumes both reached the submit stage — where the
        // loser's revert writes `Failed` over the winner's `Confirmed`.
        let dir = tempfile::TempDir::new().unwrap();
        let hash = format!("0x{}", "ab".repeat(32));
        let observed = burned_record(dir.path(), &hash);

        let mut lock = None;
        let (r, _an_tx) = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut lock,
        )
        .expect("a recorded burn resumes");
        assert_eq!(r.an_tx_hash.as_deref(), Some(hash.as_str()));
        assert!(lock.is_some(), "a resume must own the withdrawal too");

        // And a second resume, concurrent with the first, is refused
        // rather than racing it to stage 6.
        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("the first resume still holds this withdrawal");
        assert!(
            matches!(err, CliError::ReservationInFlight { .. }),
            "exit 3: {err:?}"
        );
        // And the refusal carries the verdict, not just the code: this is
        // the sentence that stops the second operator deleting a record
        // whose burn is on the wire.
        let msg = format!("{err}");
        assert!(msg.contains("RIGHT NOW"), "the live-holder verdict: {msg}");
        assert!(msg.contains("do not delete it"), "{msg}");
    }

    #[test]
    fn a_resume_whose_record_vanished_restores_it_instead_of_poisoning_it() {
        // (c), and the worst of the three. The record is deleted between
        // the stage-1 peek and the reservation — which is exactly what the
        // CLI's own exit-3 message tells operators to do once they have
        // reconciled. The run used to carry on with the peeked hash and
        // never write it back, so stage 4 persisted `Captured` with
        // `an_tx_hash: None` — a shape `read_record` refuses FOREVER as
        // "acting on it would broadcast a SECOND burn". A completed
        // withdrawal and an unrecoverable record.
        let dir = tempfile::TempDir::new().unwrap();
        let hash = format!("0x{}", "cd".repeat(32));
        let r = burned_record(dir.path(), &hash);
        let path = idempotency::record_path(dir.path(), &r.key);
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());

        let (restored, _an_tx) = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &r,
            &mut None,
        )
        .expect("the burn is known; the run must not be stranded");

        // The record is back, and it says what actually happened.
        assert_eq!(restored.an_tx_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(restored.status, Status::Burned);

        // And it is readable — the shape the guard refuses is exactly what
        // this used to leave behind.
        let reread =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .expect("a restored record must not be one read_record refuses")
                .expect("it exists");
        assert_eq!(reread.an_tx_hash.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn a_resume_acts_on_the_reservations_hash_and_not_the_peeks() {
        // Stage 1 peeks; preflight then runs for minutes. The exit-3
        // remedy an operator follows in exactly that window is "write its
        // hash into an_tx_hash and set status to burned, then re-run with
        // --allow-retry" — so the record's hash changing between the peek
        // and the reservation is not hypothetical, it is the documented
        // recovery. The reservation reads the file under the withdrawal
        // lock; the peek is stale. Capturing against the stale hash waits
        // out the timeout for an event belonging to a transaction nobody
        // is looking for.
        let dir = tempfile::TempDir::new().unwrap();
        let stale = format!("0x{}", "11".repeat(32));
        let real = format!("0x{}", "22".repeat(32));
        let mut r = burned_record(dir.path(), &stale);

        // What stage 1 read.
        let observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees the record");
        assert_eq!(observed.an_tx_hash.as_deref(), Some(stale.as_str()));

        // The operator reconciles on chain and writes the real hash in.
        r.an_tx_hash = Some(real.clone());
        idempotency::update(dir.path(), &r).unwrap();

        let (rec, an_tx) = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect("a recorded burn resumes");
        assert_eq!(
            an_tx, real,
            "the run must act on the hash the reservation read, not the one the peek saw",
        );
        assert_eq!(rec.an_tx_hash.as_deref(), Some(real.as_str()));
    }

    #[test]
    fn deleting_the_record_is_not_consent_to_resume_without_the_flag() {
        // `--allow-retry` is how an operator says "act on a withdrawal
        // that already has a record". The restore path skipped that: it
        // wrote the record back and asked only whether the status was
        // terminal, so a `burned` record deleted mid-preflight resumed
        // with no flag at all — where the identical record still on disk
        // exits 3.
        //
        // The window is not incidental. Deleting the record is what the
        // CLI's own exit-3 message tells a reconciled operator to do, so
        // the gate was being lifted by the documented recovery.
        let dir = tempfile::TempDir::new().unwrap();
        let hash = format!("0x{}", "3d".repeat(32));
        let r = burned_record(dir.path(), &hash);

        // The control: file present, no flag.
        let present = reserve_and_decide(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a recorded burn without the flag is exit 3");
        assert_eq!(present.exit_code().as_i32(), 3);

        // Same identity, same absent flag, file deleted between the peek
        // and the reservation.
        let observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees it");
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
            &observed,
            &mut None,
        )
        .expect_err("deleting the file is not the consent the flag is");
        assert_eq!(
            err.exit_code().as_i32(),
            3,
            "the same answer the file-present run got: {err}",
        );
        assert!(
            format!("{err}").contains("--allow-retry"),
            "and the same remedy: {err}",
        );

        // And the record is back on disk, so the operator has something
        // to pass the flag against.
        let reread =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .expect("the restored record must be readable")
                .expect("it exists");
        assert_eq!(reread.an_tx_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(reread.status, Status::Burned);
    }

    #[test]
    fn the_flag_still_resumes_a_record_that_was_deleted_mid_preflight() {
        // The other half: the gate is a gate, not a wall. With the flag,
        // the restored record resumes exactly as one that never went
        // missing would.
        let dir = tempfile::TempDir::new().unwrap();
        let hash = format!("0x{}", "4e".repeat(32));
        let r = burned_record(dir.path(), &hash);
        let observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .unwrap();
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();

        let (rec, an_tx) = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect("with the flag, a restored record resumes");
        assert_eq!(an_tx, hash);
        assert_eq!(rec.status, Status::Burned);
    }

    #[test]
    fn a_resume_of_a_paid_out_withdrawal_is_refused_not_downgraded() {
        // The next layer of the same defect. This branch is entered on
        // `an_tx_hash` alone — no status check — and `reserve`'s terminal
        // refusal only fires while the file is still there. Delete the
        // record during preflight, which takes minutes because it hashes a
        // 2.65 GB proving key, and a `confirmed` withdrawal came back as
        // `burned` and was carried through capture, prove and a SECOND
        // `withdrawByProof`. The concurrent route to that end state was
        // closed a round ago; this is the sequential one.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "ef".repeat(32));
        let eth = format!("0x{}", "12".repeat(32));
        let mut r = burned_record(dir.path(), &an);
        r.status = Status::Confirmed;
        r.eth_tx_hash = Some(eth.clone());
        idempotency::update(dir.path(), &r).unwrap();

        // Stage 1 read it; then it was deleted — which is exactly what the
        // CLI's own exit-3 message tells a reconciled operator to do.
        let observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees the record");
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("a withdrawal that has already paid out must not resume");
        assert_eq!(err.exit_code().as_i32(), 3, "{err:?}");
        let msg = format!("{err}");
        assert!(
            msg.contains("already paid out"),
            "the remedy a terminal record earns: {msg}",
        );

        // And the record is back as it was. Not `burned`, and carrying the
        // EVM hash an operator reconciles against — restoring only the
        // status would still have dropped that.
        let reread =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .expect("the restored record must be one read_record accepts")
                .expect("it exists");
        assert_eq!(
            reread.status,
            Status::Confirmed,
            "a fixed status here is the defect",
        );
        assert_eq!(reread.eth_tx_hash.as_deref(), Some(eth.as_str()));
        assert_eq!(reread.an_tx_hash.as_deref(), Some(an.as_str()));
    }

    #[test]
    fn a_lockless_filesystem_is_never_told_the_withdrawal_is_free() {
        // The wiring, not the mapping. `holder_verdict` has had a test
        // since the enum landed, and `decide_burn` has had one since last
        // round — but nothing ran the path BETWEEN them, so the hand-
        // written pair that fed `decide_burn` could be flipped from
        // `false` to `true` with the whole suite green. A lockless mount
        // then printed "the record was left by a run that has already
        // exited", which is the row the runbook turns into permission to
        // delete, about a record another run may be mid-send on.
        let dir = tempfile::TempDir::new().unwrap();
        // A reservation already on disk with no hash: the shape
        // `decide_burn` refuses, and the one whose refusal carries the
        // liveness line.
        idempotency::reserve(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect("the first reservation is uncontested");

        let err = reserve_and_decide_holding(
            idempotency::LockAttempt::Unsupported {
                why: "No locks available (os error 37)".into(),
            },
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            false,
        )
        .expect_err("a hash-less reservation must refuse whatever the lock could say");

        let msg = format!("{err}");
        assert_eq!(err.exit_code().as_i32(), 3, "{err}");
        assert!(
            msg.contains("could not be determined"),
            "an attempt that could not be made says so: {msg}",
        );
        assert!(
            !msg.contains("No other process"),
            "the one sentence a run with no lock evidence may never produce — it is what \
             authorises deleting the record: {msg}",
        );
    }

    #[test]
    fn the_first_line_of_the_resume_arm_cannot_report_that_nothing_was_sent() {
        // The escape that survived the round that was supposed to close
        // it. The fix landed on the `reserve` call deep inside this
        // function — a branch its own test documents as unreachable by
        // construction today — while the function's FIRST statement,
        // executed on every resume, kept propagating exit 2.
        //
        // The lever is the withdrawal lock: a directory where the lock
        // file goes makes `try_acquire`'s `open` fail with EISDIR for
        // every uid, after `ensure_state_dir` has already succeeded. That
        // is one of four `CliError::Preflight` sources behind the single
        // `?` on the first line, and the one that carries
        // `NOTHING_SENT_CLAUSE` in its own text — so this pins the
        // re-badging and the clause's removal in one run.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "cd".repeat(32));
        let observed = burned_record(dir.path(), &an);

        let lock_path = dir.path().join(format!("{}.lock", observed.key));
        // `burned_record` reserved, so `try_acquire` has already created it.
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::create_dir(&lock_path).unwrap();

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("a lock that cannot be opened is still a refusal");

        assert_eq!(
            err.exit_code().as_i32(),
            10,
            "entry to this function is gated on a burn being on the wire, so its first line is \
             already downstream of the send: {err}",
        );
        let msg = format!("{err}");
        assert!(
            !msg.to_ascii_lowercase().contains("nothing was sent"),
            "the inner refusal's pre-send clause has to come back out, or the message contradicts \
             itself: {msg}",
        );
        assert!(
            msg.contains(&an) && !msg.contains("Some("),
            "the hash is what the operator pastes into an explorer; `{{:?}}` on the Option \
             wrapped it in `Some(\"…\")`: {msg}",
        );
    }

    #[test]
    fn no_pre_send_advice_survives_the_resume_arm() {
        // `resumed_refusal` strips ONE clause, and stripping is the wrong
        // shape for the problem: `reserve`, `try_acquire` and
        // `read_record` are all reachable from a caller whose burn is on
        // the wire, and any pre-send advice any of them grows rides
        // straight into an exit-10 message. `read_record`'s corrupt-record
        // refusal used to end "delete it manually if you know it's stale",
        // which on this path is an instruction to delete the only record
        // of a live withdrawal.
        //
        // So this is the invariant, checked against the real messages
        // rather than a list of phrases to remove: nothing an operator can
        // be told by these refusals may read as permission to delete.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "ef".repeat(32));
        let observed = burned_record(dir.path(), &an);

        // A lock that cannot be opened — the reachable Preflight source
        // behind the resume arm's first statement.
        let lock_path = dir.path().join(format!("{}.lock", observed.key));
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::create_dir(&lock_path).unwrap();
        let from_the_lock = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("a lock that cannot be opened is still a refusal");

        // And the corrupt-record refusal, reached the way the pipeline
        // reaches it — `peek` reads the file — then re-badged through the
        // same function, which is how it would land on an operator here.
        std::fs::write(
            idempotency::record_path(dir.path(), &observed.key),
            b"{ this is not json",
        )
        .unwrap();
        let torn = idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
            .expect_err("a torn record is a refusal, not an absent one");
        let from_the_record = resumed_refusal(torn, &observed);

        for err in [from_the_lock, from_the_record] {
            let msg = format!("{err}").to_ascii_lowercase();
            assert_eq!(err.exit_code().as_i32(), 10, "{err}");
            for advice in [
                "delete it manually",
                "if you know it's stale",
                "nothing was sent",
            ] {
                assert!(
                    !msg.contains(advice),
                    "a refusal about a live burn told the operator {advice:?}: {msg}",
                );
            }
        }
    }

    #[test]
    fn a_reservation_that_fails_while_resuming_is_not_reported_as_having_sent_nothing() {
        // The escape this arm shipped with. `reserve` renders its own
        // failures as pre-send refusals — correct at its other call site,
        // and a lie at this one, where entry is gated on a peeked
        // `an_tx_hash` and this run has already written the record back.
        //
        // Reaching `reserve`'s refusal from in here needs the restored
        // record to be a shape `read_record` rejects, and the only such
        // shape is an active status with no hash — so this hands
        // `resume_recorded_burn` an `observed` the caller's own filter
        // would never produce. That is the point: the branch is
        // unreachable by construction TODAY, and the exit code it carries
        // is what a future caller inherits. Two is the answer that gets a
        // second burn fired by a retry wrapper.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "ab".repeat(32));
        let r = burned_record(dir.path(), &an);

        let mut observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees the record");
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();
        // Restored verbatim, so this is what `reserve` will read back.
        observed.an_tx_hash = None;

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("a record read_record rejects cannot resume");

        assert_eq!(
            err.exit_code().as_i32(),
            10,
            "every line in this arm is downstream of a burn: {err}",
        );
        let msg = format!("{err}").to_ascii_lowercase();
        assert!(
            !msg.contains("nothing was sent"),
            "the sentence a retry wrapper acts on: {msg}",
        );
        assert!(
            msg.contains("case 3a"),
            "exit 10's remedy is reconciliation, and the message has to say where: {msg}",
        );
    }

    #[test]
    fn a_restored_record_that_comes_back_without_a_hash_refuses_as_post_send() {
        // The `else` at the end of the resume arm, which nothing reached.
        // It fires when the reservation hands back a record carrying no
        // AN tx hash — and the only way there is an `observed` whose
        // status `read_record` accepts without one, i.e. `Reserved`.
        // The caller's filter never produces that; this function is the
        // unit under test, and the exit code the branch carries is what a
        // future caller inherits.
        //
        // The point is the CODE, not the prose: a run that has lost the
        // hash it was resuming from has a burn on the wire it can no
        // longer name, and exit 2 there invites a retry wrapper to make a
        // second one.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "ba".repeat(32));
        let r = burned_record(dir.path(), &an);

        let mut observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees the record");
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();
        // A shape `read_record` accepts: no hash, and a status that does
        // not require one.
        observed.an_tx_hash = None;
        observed.status = Status::Reserved;

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("there is no burn to resume from");

        assert_eq!(
            err.exit_code().as_i32(),
            10,
            "every line in this arm is downstream of a burn: {err}",
        );
        let msg = format!("{err}");
        assert!(
            msg.contains("no burn to resume from") && msg.contains("Reconcile"),
            "the operator is left reconciling, which is the only move available: {msg}",
        );
        assert!(
            !msg.to_ascii_lowercase().contains("nothing was sent"),
            "the sentence a retry wrapper acts on: {msg}",
        );
    }

    #[test]
    fn a_duplicate_refusal_still_comes_out_of_the_resume_path_unchanged() {
        // The other half of the same mapping, and the reason it is not a
        // catch-all: `reserve`'s exit-3 refusals are the entire purpose of
        // asking it here. Re-badging them as exit 10 would hide the
        // duplicate and hand the operator a reconciliation task instead of
        // the per-status remedy.
        //
        // `a_terminal_record_deleted_mid_preflight_still_refuses` drives
        // the same seam for the confirmed case; this one pins that the
        // pass-through survives the `map_err` added beside it.
        let dir = tempfile::TempDir::new().unwrap();
        let an = format!("0x{}", "cd".repeat(32));
        let mut r = burned_record(dir.path(), &an);
        r.status = Status::Submitted;
        r.eth_tx_hash = Some(format!("0x{}", "34".repeat(32)));
        idempotency::update(dir.path(), &r).unwrap();

        let observed =
            idempotency::peek(dir.path(), &seam_from(), &seam_to(), &UsdcAmount(1_000_000))
                .unwrap()
                .expect("stage 1 sees the record");
        std::fs::remove_file(idempotency::record_path(dir.path(), &r.key)).unwrap();

        let err = resume_recorded_burn(
            dir.path(),
            &seam_from(),
            &seam_to(),
            &UsdcAmount(1_000_000),
            true,
            &observed,
            &mut None,
        )
        .expect_err("a submitted withdrawal must not resume");
        assert_eq!(
            err.exit_code().as_i32(),
            3,
            "the duplicate refusal must reach the operator as itself: {err}",
        );
    }

    #[test]
    fn only_one_of_two_racing_runs_is_allowed_to_burn() {
        // The end of the argument. Threads, not a scripted sequence: this
        // is the shape that produced the defect, and it is the shape a
        // future refactor would have to keep passing. Whichever thread wins
        // the atomic publish gets `Send`; the other gets exit 3. Never two
        // sends, never two refusals.
        use std::sync::{Arc, Barrier};

        for attempt in 0..40 {
            let dir = tempfile::TempDir::new().unwrap();
            let path = Arc::new(dir.path().to_path_buf());
            let gate = Arc::new(Barrier::new(2));

            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = path.clone();
                    let gate = gate.clone();
                    std::thread::spawn(move || {
                        gate.wait();
                        reserve_and_decide(
                            &path,
                            &seam_from(),
                            &seam_to(),
                            &UsdcAmount(1_000_000),
                            true,
                        )
                    })
                })
                .collect();

            let mut sends = 0;
            let mut refusals = 0;
            for h in handles {
                match h.join().unwrap() {
                    Ok((_, BurnDecision::Send, _lock)) => sends += 1,
                    Ok((_, BurnDecision::Reuse(h), _lock)) => {
                        panic!("attempt {attempt}: nothing has been sent, so nothing to reuse: {h}")
                    },
                    Err(CliError::ReservationInFlight {
                        ..
                    }) => refusals += 1,
                    Err(e) => panic!("attempt {attempt}: unexpected refusal: {e:?}"),
                }
            }
            assert_eq!(sends, 1, "attempt {attempt}: exactly one run may burn");
            assert_eq!(refusals, 1, "attempt {attempt}: the other must exit 3");
        }
    }

    // `assert!(matches!(..))`, not a bare `matches!(..)`. In statement
    // position the macro's bool is discarded, so these two asserted only
    // that `parse_anchor_layer` returned Ok and did not panic — verified
    // empirically: they passed against a wrong variant.
    #[test]
    fn parse_anchor_layer_auto() {
        assert!(matches!(
            parse_anchor_layer("auto").unwrap(),
            AnchorLayerMode::Auto
        ));
        assert!(matches!(
            parse_anchor_layer("AUTO").unwrap(),
            AnchorLayerMode::Auto
        ));
    }

    #[test]
    fn parse_anchor_layer_explicit() {
        assert!(matches!(
            parse_anchor_layer("2").unwrap(),
            AnchorLayerMode::Explicit(2)
        ));
        // Pin the number, not just the variant: `Explicit(1)` and
        // `Explicit(2)` are ~6 min and ~91 min of anchor wait.
        assert!(matches!(
            parse_anchor_layer("1").unwrap(),
            AnchorLayerMode::Explicit(1)
        ));
    }

    #[test]
    fn parse_anchor_layer_zero_rejects() {
        assert!(parse_anchor_layer("0").is_err());
    }

    #[test]
    fn parse_anchor_layer_junk_rejects() {
        assert!(parse_anchor_layer("nope").is_err());
    }

    #[test]
    fn split_extended_ok() {
        let (a, b) = split_extended("aa::bb").unwrap();
        assert_eq!(a, "aa");
        assert_eq!(b, "bb");
    }

    #[test]
    fn split_extended_missing_sep_none() {
        assert!(split_extended("no-sep").is_none());
    }
}
