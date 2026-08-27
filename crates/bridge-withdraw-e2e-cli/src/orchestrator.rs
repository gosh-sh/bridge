//! Top-level orchestration. Composes the six pipeline stages behind a
//! single async entrypoint that `main` invokes.
//!
//! Stages, error-code-wise:
//! 1. **Preflight** ([`crate::preflight`]) — exit 2 on refusal.
//! 2. **Idempotency** ([`crate::idempotency`]) — exit 3 on duplicate.
//! 3. **Burn** ([`crate::burn`]) — exit 10 on unknown-outcome mid-send.
//! 4. **Capture** — reuses [`bridge_relayer_daemon::withdraw_e2e::run_once`]
//!    with `replay_latest = true`. Since we just fired our own burn, the
//!    youngest matching WithdrawalInitiated from `--from` is unambiguously
//!    ours; snapshotting a baseline BEFORE the burn and threading it into
//!    `run_once` would require reimplementing the export → enrich → prove
//!    pipeline manually. Exit 11 on capture timeout, exit 12 on prover
//!    failure — both bubble out of the same `run_once` call.
//! 5. **Submit** — reuses [`bridge_relayer_daemon::bridge::EthBridgeClient`]
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
use bridge_relayer_daemon::withdraw_e2e::{run_once, WithdrawE2EConfig};

use crate::args::{self, WithdrawArgs};
use crate::burn;
use crate::errors::{CliError, CliResult};
use crate::idempotency::{self, Status};
use crate::preflight;
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
pub async fn run(args: WithdrawArgs, dry_run: bool) -> CliResult<WithdrawSuccess> {
    // ---- Arg validation (parses raw strings into typed forms) ----
    let from = args::parse_from(&args.from)?;
    let to = args::parse_to(&args.to, args.to_chain)?;
    let amount = args::parse_amount(&args.amount)?;
    let anchor_mode = parse_anchor_layer(&args.anchor_layer)?;

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

    // ---- 3. Burn ----
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

    // ---- 4+5. Capture + Prove (both wrapped in run_once) ----
    // Split the "dapp_id::account_id" the preflight resolved for USDCBridge
    // back into its two halves — run_once wants them separately.
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

    info!("stage 4/6 + 5/6: capture WithdrawalInitiated + Circuit-4 SHPLONK proof");
    let e2e_cfg = WithdrawE2EConfig {
        gql_endpoint: args.gql_endpoint.clone(),
        prover_state_path: args.prover_state_path.clone(),
        window_size: args.window_size,
        bridge_account_id_hex,
        bridge_dapp_id_hex,
        event_dst_filter: DEFAULT_EVENT_DST.to_string(),
        event_wait: EVENT_WAIT,
        event_poll_interval: EVENT_POLL_INTERVAL,
        anchor_mode,
        i_know_the_wait: args.i_know_the_wait,
        work_dir: args.work_dir.clone(),
        aggregator_dir: args.aggregator_dir.clone(),
        verifiers_dir: args.verifiers_dir.clone(),
        params_dir: args.params_dir.clone(),
        snark_dir: args.snark_dir.clone(),
        pk_cache_dir: args.pk_cache_dir.clone(),
        prover_out_dir: args.prover_out_dir.clone(),
        prover_timeout: Duration::from_secs(args.prover_timeout_s),
        prover_seq_no: 0,
        // We just fired a burn; the youngest matching WithdrawalInitiated
        // from --from is unambiguously ours. See module docstring.
        replay_latest: true,
    };
    let e2e = run_once(e2e_cfg).await.map_err(|e| {
        // Anyhow chain -> ProofFailed (exit 12). Capture-timeout also
        // bubbles up here since capture is the first step in run_once; we
        // fold it into ProofFailed for v1 rather than trying to string-match
        // the anyhow chain. v2 can split the two if the error taxonomy
        // matters to consumers.
        CliError::ProofFailed {
            reason: format!("withdraw-e2e pipeline failed: {e}"),
            source: Some(e),
        }
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
    let signer: PrivateKeySigner = args
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

    if let Some(r) = record.as_mut() {
        r.status = Status::Submitted;
        idempotency::update(&state_dir, r)?;
    }

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
            SubmitSummary {
                eth_tx: Some(tx),
                status: SubmitStatus::Confirmed,
            }
        }
        WithdrawSubmitOutcome::Reverted { reason } => {
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
            an_tx: burn_receipt.an_tx_hash,
            bounce: burn_receipt.bounce,
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
