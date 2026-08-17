//! `run_once` — thread capture → hermetic export → daemon-side
//! enrichment → subprocess Halo2 prover into a single async call.
//!
//! Mirrors the Rust half of `run_event_proving_steps` in
//! `python/helper/bridge_e2e.py` (steps 5–7), but without shelling out
//! to the exporter and enricher binaries — those two stages are library
//! calls now (`bridge_event_witness::export_from_event_boc_base64` /
//! `bridge_event_witness::enrich_witness`).
//!
//! ETH-side submission is left to the caller: the CLI subcommand takes
//! the returned [`WithdrawE2ESummary`], parses the proof bytes, and
//! drives `EthBridgeClient::submit_withdraw` itself. Keeping the ETH
//! wallet / provider out of this module simplifies embedding into
//! non-CLI callers (tests, higher-level loops) that don't have or want
//! a signing key.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::info;

use bridge_event_witness::{
    enrich_witness, export_from_event_boc_base64, AnchorLayerMode, BlockContextInput,
    EnrichSummary,
};
use bridge_gql_fetcher::gql_client::create_client;
use bridge_prover_lib::bridge_state::BridgeState;

use crate::withdraw_prover::{
    SubprocessWithdrawalProver, SubprocessWithdrawalProverConfig, WithdrawalProver,
};
use crate::withdrawal::PartnerWithdrawalProof;

use super::capture::{
    capture_next_withdrawal_event, snapshot_baseline_msg_ids, CapturedEvent,
};

/// One `run_once` invocation's inputs.
///
/// Grouped in a config struct rather than as positional args because the
/// CLI subcommand builds this by populating optional fields from clap
/// flags — a struct keeps that mapping obvious. Times are `Duration` so
/// the CLI can convert `--*-s` seconds arguments centrally.
#[derive(Debug, Clone)]
pub struct WithdrawE2EConfig {
    /// GraphQL endpoint URL (e.g. `https://shellnet.ackinacki.org/bk/v2/graphql`).
    /// Also accepted without scheme; `create_client` prepends `http://`.
    pub gql_endpoint: String,
    /// Path to the on-disk `BridgeState` snapshot the enricher will read.
    /// In production this is the `prover_state.json` written by the live
    /// prover driver.
    pub prover_state_path: PathBuf,
    /// `HISTORY_PROOF_WINDOW_SIZE`. Must match the value the state file
    /// was written with, otherwise `BridgeState::load` refuses.
    pub window_size: usize,
    /// Emitting account. 64-char hex, no `0x`.
    pub bridge_account_id_hex: String,
    pub bridge_dapp_id_hex: String,
    /// ExtOut `dst` filter — for WithdrawalInitiated this is
    /// `":000…026a"` (`makeAddrExtern(618)`).
    pub event_dst_filter: String,
    /// Total time budget for the capture stage.
    pub event_wait: Duration,
    /// Per-poll sleep during capture.
    pub event_poll_interval: Duration,
    pub anchor_mode: AnchorLayerMode,
    /// Ack the L(n≥2) wait budget for `AnchorLayerMode::Explicit`. Auto
    /// mode counts as an implicit ack.
    pub i_know_the_wait: bool,
    /// Where to write the enriched witness JSON.
    pub work_dir: PathBuf,
    /// `crates/an-bridge-prover` workspace root. The subprocess prover
    /// looks for a release binary here (or falls back to `cargo run`).
    pub an_bridge_prover_dir: PathBuf,
    /// Optional override for the prover subprocess's working directory
    /// (i.e. where it expects `./params/*`). Defaults to
    /// `an_bridge_prover_dir` if `None`.
    pub prover_work_dir: Option<PathBuf>,
    /// Optional dir to persist `proof_event_{seq:06}.json` alongside
    /// the returned in-memory summary.
    pub prover_out_dir: Option<PathBuf>,
    /// Subprocess timeout (Circuit 4 keygen+prove from cold PK can take
    /// minutes; default generously).
    pub prover_timeout: Duration,
    /// Seqno stamped into the summary + witness/proof filenames.
    pub prover_seq_no: u32,
}

/// Everything `run_once` produced, in one bundle. The CLI logs the
/// summary + captured event, then converts `proof` into ETH calldata via
/// `PartnerWithdrawalProof::proof_bytes()` / `public_inputs()`.
#[derive(Debug, Clone)]
pub struct WithdrawE2ESummary {
    pub captured: CapturedEvent,
    pub enrich: EnrichSummary,
    pub witness_path: PathBuf,
    pub proof: PartnerWithdrawalProof,
}

pub async fn run_once(cfg: WithdrawE2EConfig) -> Result<WithdrawE2ESummary> {
    std::fs::create_dir_all(&cfg.work_dir)
        .with_context(|| format!("mkdir work_dir {}", cfg.work_dir.display()))?;

    let gql = create_client(&cfg.gql_endpoint).context("create GqlClient")?;
    let state_path_str = cfg
        .prover_state_path
        .to_str()
        .context("prover_state_path is not valid UTF-8")?;
    let bridge_state = BridgeState::load(state_path_str, cfg.window_size)
        .with_context(|| format!("load BridgeState from {state_path_str}"))?;
    info!(
        window_size = bridge_state.window_size,
        num_active_layers = bridge_state.num_active_layers(),
        stored_last_seen_block_seq_no = bridge_state.stored_last_seen_block_seq_no,
        "loaded BridgeState",
    );

    let baseline = snapshot_baseline_msg_ids(
        &gql,
        &cfg.bridge_account_id_hex,
        &cfg.bridge_dapp_id_hex,
        500,
    )
    .await
    .context("baseline ExtOut snapshot")?;

    let captured = capture_next_withdrawal_event(
        &gql,
        &cfg.bridge_account_id_hex,
        &cfg.bridge_dapp_id_hex,
        &cfg.event_dst_filter,
        &baseline,
        cfg.event_wait,
        cfg.event_poll_interval,
    )
    .await
    .context("capture WithdrawalInitiated event")?;

    let ctx = BlockContextInput {
        block_id: parse_hex32(&captured.block_id_hex).context("block_id_hex")?,
        block_seq_no: captured.block_seq_no,
        account_dapp_id: parse_hex32(&captured.account_dapp_id_hex)
            .context("account_dapp_id_hex")?,
        account_id: parse_hex32(&captured.account_id_hex).context("account_id_hex")?,
        envelope_hash: parse_hex32(&captured.envelope_hash_hex).context("envelope_hash_hex")?,
    };
    info!("exporter: building partial PrivateWitness from ExtOut BOC");
    let partial = export_from_event_boc_base64(&captured.event_boc_b64, &ctx)
        .context("export_from_event_boc_base64 failed")?;

    info!(
        anchor_mode = ?cfg.anchor_mode,
        i_know_the_wait = cfg.i_know_the_wait,
        "enricher: filling events_tree_proof + block_tree_proof + anchor",
    );
    let enriched = enrich_witness(
        &gql,
        &bridge_state,
        partial,
        cfg.anchor_mode,
        cfg.i_know_the_wait,
    )
    .await
    .context("enrich_witness failed")?;
    info!(
        layer_idx = enriched.summary.layer_idx,
        key_block_seq_no = enriched.summary.key_block_seq_no,
        thinned_key_block_seq_no = enriched.summary.thinned_key_block_seq_no,
        auto_escalated = enriched.summary.auto_escalated,
        num_active_chain_steps = enriched.summary.num_active_chain_steps,
        "enricher: witness ready",
    );

    let witness_path = cfg
        .work_dir
        .join(format!("event_{:06}_witness.json", cfg.prover_seq_no));
    let file = std::fs::File::create(&witness_path)
        .with_context(|| format!("create witness file {}", witness_path.display()))?;
    serde_json::to_writer_pretty(file, &enriched.witness)
        .context("serialize PrivateWitness to JSON")?;
    info!("wrote enriched witness: {}", witness_path.display());

    let mut prover_cfg = SubprocessWithdrawalProverConfig::new(&cfg.an_bridge_prover_dir);
    if let Some(w) = cfg.prover_work_dir.as_ref() {
        prover_cfg.work_dir = w.clone();
    }
    prover_cfg.out_dir = cfg.prover_out_dir.clone();
    prover_cfg.seq_no = cfg.prover_seq_no;
    prover_cfg.timeout = cfg.prover_timeout;
    let prover = SubprocessWithdrawalProver::new(prover_cfg);

    info!(
        an_bridge_prover_dir = %cfg.an_bridge_prover_dir.display(),
        witness = %witness_path.display(),
        "invoking bridge-event-halo2-prover",
    );
    let proof = prover
        .prove(&witness_path)
        .await
        .context("SubprocessWithdrawalProver::prove failed")?;
    info!(
        seq_no = proof.seq_no,
        self_verified = proof.self_verified,
        pi_len = proof.public_instances_hex.len(),
        "prover produced PartnerWithdrawalProof",
    );

    Ok(WithdrawE2ESummary {
        captured,
        enrich: enriched.summary,
        witness_path,
        proof,
    })
}

/// Decode a 32-byte hex string (optional `0x` prefix) into `[u8; 32]`.
fn parse_hex32(s: &str) -> Result<[u8; 32]> {
    let trimmed = s.trim();
    let no_prefix = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes =
        hex::decode(no_prefix).with_context(|| format!("hex-decode {no_prefix:?}"))?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse_hex32;

    #[test]
    fn parse_hex32_accepts_both_prefixed_and_bare() {
        let raw = "01".repeat(32);
        let prefixed = format!("0x{raw}");
        let a = parse_hex32(&raw).unwrap();
        let b = parse_hex32(&prefixed).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0], 1);
        assert_eq!(a[31], 1);
    }

    #[test]
    fn parse_hex32_rejects_short_hex() {
        let err = parse_hex32(&"aa".repeat(31)).unwrap_err();
        assert!(err.to_string().contains("expected 32 bytes"), "{err}");
    }

    #[test]
    fn parse_hex32_rejects_bad_hex() {
        assert!(parse_hex32("zz".repeat(32).as_str()).is_err());
    }
}
