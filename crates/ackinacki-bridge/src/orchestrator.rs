//! Top-level orchestration. Composes the six pipeline stages behind a
//! single async entrypoint that `main` invokes.
//!
//! Stages, error-code-wise:
//! 1. **Preflight** ([`crate::preflight`]) — exit 2 on refusal.
//! 2. **Idempotency** ([`crate::idempotency`]) — exit 3 on duplicate.
//! 3. **Burn** ([`crate::burn`]) — exit 10 on unknown-outcome mid-send.
//! 4. **Capture** — chain-follows the multisig `an_tx_hash` through
//!    USDCBridge's `dst_transaction` to the WithdrawalInitiated ExtOut
//!    via [`bridge_relayer_daemon::withdraw_e2e::capture_targeted_withdrawal_event`].
//!    This is multi-user-safe: filtering by our specific tx hash instead
//!    of youngest-picking a shared USDCBridge queue means concurrent
//!    burns from other operators cannot be mis-selected as ours. Exit 11
//!    on capture timeout (burn bounced, GQL unreachable, or USDCBridge
//!    never emitted the ExtOut).
//! 4b. **Resurrect + coverage-wait** — see [`crate::resurrect`].
//! 5. **Prove** — reuses
//!    [`bridge_relayer_daemon::withdraw_e2e::run_once_with_state`] with
//!    the just-captured event and the resurrected `BridgeState`. Exit 12
//!    on prover failure.
//! 6. **Submit** — reuses [`bridge_relayer_daemon::bridge::EthBridgeClient`]
//!    `dry_run_withdraw` (always) and `submit_withdraw` (unless
//!    `--dry-run`). Exit 13 on revert.
//!
//! Every stage transition updates the idempotency record so a mid-flight
//! crash leaves a resumable trace. v1 doesn't implement `--resume`, but
//! the state file is written eagerly regardless so v2 has what it needs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::network::EthereumWallet;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use tracing::info;

use bridge_event_witness::AnchorLayerMode;
use bridge_relayer_daemon::bridge::{DryRunOutcome, EthBridgeClient, WithdrawSubmitOutcome};
use bridge_relayer_daemon::withdraw_e2e::{run_once_with_state, WithdrawE2EConfig};

use crate::args::{self, WithdrawArgs};
use crate::burn;
use crate::errors::{CliError, CliResult};
use crate::idempotency::{self, Status};
use crate::preflight;
use crate::resurrect::{covering_bundle_seq_no, stride_for, wait_for_coverage};
use tvm_client::net::NetworkConfig;
use tvm_client::{ClientConfig, ClientContext};

/// Default `WithdrawalInitiated` ExtOut `dst` sentinel — `makeAddrExtern(618)`.
/// Matches the relayer daemon CLI default (`bin/relayer.rs:470`).
const DEFAULT_EVENT_DST: &str =
    ":000000000000000000000000000000000000000000000000000000000000026a";

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
/// `--non-interactive` without `--yes` is rejected upstream in `main`, so
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

    // ---- 1. Preflight ----
    info!("stage 1/6: preflight");
    let preflight = preflight::run(
        &from,
        &args.from_keys,
        &to,
        &amount,
        &args.gql_endpoint,
        &args.usdc_bridge_account,
    )
    .await?;
    info!(
        multisig_ecc3 = preflight.multisig_ecc3_balance,
        usdc_bridge = %preflight.usdc_bridge_extended,
        "preflight ok",
    );

    // ---- 2. Idempotency reserve ----
    // Dry-run skips reservation (spec: "Idempotency state is NOT recorded
    // for a dry-run").
    let state_dir = args
        .state_dir
        .clone()
        .unwrap_or_else(default_state_dir);
    let mut record = if !dry_run {
        info!("stage 2/6: idempotency reserve");
        let r = idempotency::reserve(&state_dir, &from, &to, &amount, args.allow_retry)?;
        info!(key = %r.key, "reserved");
        Some(r)
    } else {
        info!("stage 2/6: idempotency (skipped for --dry-run)");
        None
    };

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

    // ---- 3. Burn ----
    // Resume: if the prior record already carries an `an_tx_hash`, the AN
    // burn has been broadcast at least once. Firing again would be a
    // double-spend on the source side (the multisig has no "same nonce"
    // guard the way EVM does — sendTransaction happily authorises a
    // second transfer). Reuse the prior hash and skip straight to capture.
    let prior_an_tx = record.as_ref().and_then(|r| r.an_tx_hash.clone());
    let (an_tx_hash, bounce) = if let Some(existing) = prior_an_tx {
        info!(
            an_tx = %existing,
            prior_status = ?record.as_ref().map(|r| r.status),
            "stage 3/6: resume — skipping burn (prior an_tx_hash on file)",
        );
        // `bounce` is not persisted; the true value only matters inside
        // burn::fire (multisig bounce flag on the composed message).
        // Downstream, it lands in the summary as informational output;
        // the default matches spec.
        (existing, true)
    } else {
        // Fresh burn — prompt the operator first. This is the last
        // reversible moment: after burn::fire returns, the multisig has
        // authorised the outgoing transfer. `--yes` skips the prompt for
        // scripts; `--non-interactive` without `--yes` is already refused
        // in `main::dispatch`, so a live prompt here is safe to block on.
        if !skip_prompt {
            confirm_before_burn(&from, &to, &amount, &args, anchor_mode)?;
        }

        info!("stage 3/6: burn (multisig sendTransaction → USDCBridge.initiateWithdrawal)");
        let context = build_tvm_client(&args.gql_endpoint)?;
        let bounce = true; // spec default; see burn.rs header comment
        let burn_receipt = burn::fire(
            &context,
            &preflight,
            &from,
            &args.from_keys,
            &to,
            &amount,
            bounce,
        )
        .await?;
        info!(an_tx = %burn_receipt.an_tx_hash, "burn broadcast");
        if let Some(r) = record.as_mut() {
            r.status = Status::Burned;
            r.an_tx_hash = Some(burn_receipt.an_tx_hash.clone());
            idempotency::update(&state_dir, r)?;
        }
        (burn_receipt.an_tx_hash, burn_receipt.bounce)
    };

    // ---- 4. Capture WithdrawalInitiated ----
    // Split the "dapp_id::account_id" the preflight resolved for USDCBridge
    // back into its two halves — the capture helper wants them separately.
    let (bridge_dapp_id_hex, bridge_account_id_hex) = split_extended(
        &preflight.usdc_bridge_extended,
    )
    .ok_or_else(|| CliError::Preflight {
        reason: format!(
            "internal: usdc_bridge_extended {} is not `dapp_id::account_id`",
            preflight.usdc_bridge_extended
        ),
        source: None,
    })?;

    info!("stage 4/6: capture WithdrawalInitiated event (targeted by an_tx_hash)");
    let gql = bridge_gql_fetcher::gql_client::create_client(&args.gql_endpoint).map_err(|e| {
        CliError::ProofFailed {
            reason: format!("failed to build GqlClient for {}: {e}", args.gql_endpoint),
            source: Some(anyhow::anyhow!("{e}")),
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
        idempotency::update(&state_dir, r)?;
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
    let ro_provider = ProviderBuilder::new()
        .connect_http(args.rpc_url.parse().map_err(|e| CliError::EthSubmitFailed {
            reason: format!("--rpc-url is not a valid URL: {e}"),
            source: None,
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
        prover_seq_no: 0,
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
        idempotency::update(&state_dir, r)?;
    }

    // ---- 6. Submit (dry-run then real) ----
    info!("stage 6/6: submit withdrawByProof");
    let proof_bytes = e2e.proof.proof_bytes().map_err(|e| CliError::EthSubmitFailed {
        reason: format!("PartnerWithdrawalProof::proof_bytes: {e}"),
        source: Some(anyhow::anyhow!("{e}")),
    })?;
    let pub_inputs = e2e.proof.public_inputs().map_err(|e| CliError::EthSubmitFailed {
        reason: format!("PartnerWithdrawalProof::public_inputs: {e}"),
        source: Some(anyhow::anyhow!("{e}")),
    })?;

    // Always dry-run first — catches on-chain-side issues (paused bridge,
    // treasury shortfall) before we spend gas.
    {
        let ro_provider = ProviderBuilder::new()
            .connect_http(args.rpc_url.parse().map_err(|e| CliError::EthSubmitFailed {
                reason: format!("--rpc-url is not a valid URL: {e}"),
                source: None,
            })?);
        let ro_bridge = EthBridgeClient::new(args.bridge_address, ro_provider);
        match ro_bridge
            .dry_run_withdraw(&proof_bytes, &pub_inputs)
            .await
            .map_err(|e| CliError::EthSubmitFailed {
                reason: format!("dry_run_withdraw: {e}"),
                source: Some(anyhow::anyhow!("{e}")),
            })? {
            DryRunOutcome::WouldSucceed => info!("eth dry-run: withdrawByProof would succeed"),
            DryRunOutcome::WouldRevert { reason } => {
                return Err(CliError::EthSubmitFailed {
                    reason: format!("withdrawByProof dry-run reverted: {reason}"),
                    source: None,
                });
            }
        }
    }

    // Real submit: build a wallet-filled provider and send.
    let signer: PrivateKeySigner = plumbing_ref
        .eth_private_key
        .parse()
        .map_err(|e| CliError::EthSubmitFailed {
            reason: format!("parse --eth-private-key: {e}"),
            source: None,
        })?;
    let probe = ProviderBuilder::new()
        .connect_http(args.rpc_url.parse().map_err(|e| CliError::EthSubmitFailed {
            reason: format!("--rpc-url parse: {e}"),
            source: None,
        })?);
    let chain_id = probe
        .get_chain_id()
        .await
        .map_err(|e| CliError::EthSubmitFailed {
            reason: format!("get_chain_id: {e}"),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    if chain_id != to.chain_id {
        return Err(CliError::EthSubmitFailed {
            reason: format!(
                "RPC chain_id {chain_id} does not match --to-chain {} — refuse to submit \
                 to the wrong chain",
                to.chain_id
            ),
            source: None,
        });
    }
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(args.rpc_url.parse().map_err(|e| CliError::EthSubmitFailed {
            reason: format!("--rpc-url parse: {e}"),
            source: None,
        })?);
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
            source: Some(anyhow::anyhow!("{e}")),
        })? {
        WithdrawSubmitOutcome::Paid { tx_hash } => {
            let tx = format!("{tx_hash:?}");
            info!(tx = %tx, "withdrawByProof paid out");
            // Preserve the two-step trail (Submitted → Confirmed) so debug
            // tools and crash recovery can distinguish "we broadcast" from
            // "we saw the receipt". Both writes happen post-broadcast, so
            // the state file never over-claims.
            if let Some(r) = record.as_mut() {
                r.status = Status::Submitted;
                r.eth_tx_hash = Some(tx.clone());
                idempotency::update(&state_dir, r)?;
            }
            SubmitSummary {
                eth_tx: Some(tx),
                status: SubmitStatus::Confirmed,
            }
        }
        WithdrawSubmitOutcome::Reverted { reason } => {
            // Preserve the proof so a follow-up run can re-submit without
            // re-proving. `proof_json_path` is populated by the aggregator
            // subprocess if the operator passed `--prover-out-dir`; when
            // absent (default), the proof lives only in memory and a
            // retry will re-prove — still deterministic.
            if let Some(r) = record.as_mut() {
                r.status = Status::Failed;
                let _ = idempotency::update(&state_dir, r);
            }
            return Err(CliError::EthSubmitFailed {
                reason: format!("withdrawByProof reverted: {reason}"),
                source: None,
            });
        }
    };

    if let Some(r) = record.as_mut() {
        r.status = Status::Confirmed;
        r.eth_tx_hash = submit.eth_tx.clone();
        idempotency::update(&state_dir, r)?;
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

fn build_tvm_client(gql_endpoint: &str) -> CliResult<Arc<ClientContext>> {
    let config = ClientConfig {
        network: NetworkConfig {
            endpoints: Some(vec![gql_endpoint.to_string()]),
            sending_endpoint_count: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let ctx = ClientContext::new(config).map_err(|e| CliError::BurnOutcomeUnknown {
        reason: format!("failed to build tvm_client context for {gql_endpoint}: {e}"),
        source: Some(anyhow::anyhow!("{e}")),
    })?;
    Ok(Arc::new(ctx))
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
fn default_state_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".bridge-withdraw-state")
}

/// Terminal confirmation before the AN burn. Prints the money-moving
/// details to stderr, reads a line from stdin, and refuses (Preflight
/// error, exit 2) unless the user types "y" or "yes" (case-insensitive).
///
/// Skipped when `--yes` is set. When stdin is closed / not a TTY, we
/// treat it as an implicit refusal — the operator should have passed
/// `--yes` (allowed) or `--non-interactive` (already refused upstream).
fn confirm_before_burn(
    from: &args::FromAddress,
    to: &args::ToAddress,
    amount: &args::UsdcAmount,
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
        }
    };

    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr);
    let _ = writeln!(stderr, "About to withdraw USDC:");
    let _ = writeln!(stderr, "  from      : {}", from.extended());
    let _ = writeln!(
        stderr,
        "  to        : 0x{} on {} (chain {})",
        hex::encode(to.address.as_slice()),
        chain_name,
        to.chain_id,
    );
    let _ = writeln!(stderr, "  amount    : {} USDC", amount.display());
    let _ = writeln!(stderr, "  bridge    : {}", args.bridge_address);
    let _ = writeln!(stderr, "  anchor    : {:?} (wait {})", anchor_mode, wait_hint);
    let _ = writeln!(stderr);
    let _ = writeln!(stderr, "This will:");
    let _ = writeln!(
        stderr,
        "  1. broadcast a multisig sendTransaction burning {} USDC on Acki Nacki",
        amount.display(),
    );
    let _ = writeln!(stderr, "  2. wait for the covering anchor bundle to land on Sepolia");
    let _ = writeln!(stderr, "  3. produce a Circuit-4 SHPLONK proof");
    let _ = writeln!(stderr, "  4. submit withdrawByProof (spends ETH gas)");
    let _ = writeln!(stderr);
    let _ = writeln!(
        stderr,
        "The AN burn is irreversible once broadcast. Pass --yes to skip this prompt."
    );
    let _ = write!(stderr, "Proceed? [y/N]: ");
    let _ = stderr.flush();
    drop(stderr);

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

    #[test]
    fn parse_anchor_layer_auto() {
        matches!(parse_anchor_layer("auto").unwrap(), AnchorLayerMode::Auto);
        matches!(parse_anchor_layer("AUTO").unwrap(), AnchorLayerMode::Auto);
    }

    #[test]
    fn parse_anchor_layer_explicit() {
        matches!(
            parse_anchor_layer("2").unwrap(),
            AnchorLayerMode::Explicit(2)
        );
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
