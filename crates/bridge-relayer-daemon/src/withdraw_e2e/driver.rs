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

use crate::aggregator::{
    Circuit4ShplonkPipeline, InProcessCircuit4SnarkProver, SubprocessAggregator,
    SubprocessAggregatorConfig,
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
    /// `crates/bridge-evm-aggregator` root (holds
    /// `target/release/aggregate-proof`). Forwarded to
    /// [`SubprocessAggregatorConfig`] inside the SHPLONK pipeline.
    pub aggregator_dir: PathBuf,
    /// Directory of committed verifier `.bin` files — the aggregator's
    /// byte-identity self-check target
    /// (`BridgeWithdrawalAggregatorVerifier.bin` in particular).
    pub verifiers_dir: PathBuf,
    /// Directory holding `kzg_bn254_*.srs` + Circuit-4 keys. Both the
    /// in-process Poseidon C4 prover and the aggregator subprocess read
    /// from here.
    pub params_dir: PathBuf,
    /// Scratch dir for the intermediate `circuit4.snark` /
    /// `.instances.bin` the [`Circuit4ShplonkPipeline`] emits.
    pub snark_dir: PathBuf,
    /// Optional persistent outer-PK cache directory for the
    /// `aggregate-proof` subprocess. Defaults to `<params_dir>/pk_cache`
    /// if `None`. See `daemon-live --pk-cache-dir`.
    pub pk_cache_dir: Option<PathBuf>,
    /// Optional dir to persist `proof_event_{seq:06}.json` alongside
    /// the returned in-memory summary.
    pub prover_out_dir: Option<PathBuf>,
    /// Aggregator subprocess timeout (Circuit-4 outer keygen+aggregate
    /// from cold PK can take minutes; default generously).
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

    // Enricher runs against a live-updated BridgeState — the daemon writes new
    // bundles to prover_state.json as it proves them. On a fresh deploy where
    // the covering bundle for a just-fired burn is 2+ bundles ahead of seed,
    // the first attempt fails with either "bridge state is uninitialized" or a
    // missing layer_hashes[K] anchor. Retry with periodic reloads until the
    // daemon lands the covering bundle or we exceed the wait budget.
    const ENRICH_POLL_INTERVAL: Duration = Duration::from_secs(30);
    const ENRICH_TIMEOUT: Duration = Duration::from_secs(90 * 60);
    info!(
        anchor_mode = ?cfg.anchor_mode,
        i_know_the_wait = cfg.i_know_the_wait,
        poll_interval_s = ENRICH_POLL_INTERVAL.as_secs(),
        timeout_s = ENRICH_TIMEOUT.as_secs(),
        "enricher: filling events_tree_proof + block_tree_proof + anchor",
    );
    let deadline = std::time::Instant::now() + ENRICH_TIMEOUT;
    let mut attempt: u32 = 0;
    let enriched = loop {
        attempt += 1;
        // Reload from disk — daemon writes prover_state.json each bundle.
        let bridge_state_now = BridgeState::load(state_path_str, cfg.window_size)
            .with_context(|| format!("reload BridgeState from {state_path_str}"))?;
        info!(
            attempt,
            stored_last_seen_block_seq_no = bridge_state_now.stored_last_seen_block_seq_no,
            num_active_layers = bridge_state_now.num_active_layers(),
            "enricher attempt",
        );
        match enrich_witness(
            &gql,
            &bridge_state_now,
            partial.clone(),
            cfg.anchor_mode,
            cfg.i_know_the_wait,
        )
        .await
        {
            Ok(e) => break e,
            Err(err) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Err(err).context(format!(
                        "enrich_witness failed after {} attempts (timeout {:?})",
                        attempt, ENRICH_TIMEOUT
                    ));
                }
                let remaining = deadline.duration_since(now);
                info!(
                    attempt,
                    error = %err,
                    sleep_s = ENRICH_POLL_INTERVAL.as_secs(),
                    remaining_s = remaining.as_secs(),
                    "enricher: not ready yet, will retry",
                );
                tokio::time::sleep(ENRICH_POLL_INTERVAL).await;
            }
        }
    };
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

    // Compose the SHPLONK pipeline the on-chain
    // `BridgeWithdrawalAggregatorVerifier` accepts:
    //   1. `InProcessCircuit4SnarkProver` re-proves the witness with a
    //      Poseidon transcript at K=19, writing `circuit4.snark` +
    //      `circuit4.instances.bin` into `snark_dir`.
    //   2. `SubprocessAggregator` shells out to `aggregate-proof
    //      --name BridgeWithdrawalAggregatorVerifier`, producing the
    //      22-instance SHPLONK calldata (`instances ‖ proof`) that
    //      matches the deployed Yul verifier byte-for-byte.
    // The old subprocess path via `bridge-event-halo2-prover` returned
    // a raw Blake2 halo2 proof — self-verified locally but rejected
    // on-chain as `WithdrawalProofRejected()`.
    std::fs::create_dir_all(&cfg.snark_dir)
        .with_context(|| format!("mkdir snark_dir {}", cfg.snark_dir.display()))?;
    let pk_cache_dir = cfg
        .pk_cache_dir
        .clone()
        .unwrap_or_else(|| cfg.params_dir.join("pk_cache"));
    let mut agg_cfg = SubprocessAggregatorConfig::new(
        &cfg.aggregator_dir,
        &cfg.verifiers_dir,
        &cfg.params_dir,
    )
    .with_pk_cache_dir(&pk_cache_dir);
    agg_cfg.timeout = cfg.prover_timeout;
    let snark_prover = InProcessCircuit4SnarkProver::new(&cfg.params_dir);
    let aggregator = SubprocessAggregator::new(agg_cfg);
    let pipeline = Circuit4ShplonkPipeline::new(snark_prover, aggregator);

    info!(
        aggregator_dir = %cfg.aggregator_dir.display(),
        verifiers_dir = %cfg.verifiers_dir.display(),
        params_dir = %cfg.params_dir.display(),
        snark_dir = %cfg.snark_dir.display(),
        pk_cache_dir = %pk_cache_dir.display(),
        witness = %witness_path.display(),
        "invoking Circuit4ShplonkPipeline (in-process Poseidon C4 prove → aggregate)",
    );
    let proof = pipeline
        .prove(&witness_path, &cfg.snark_dir, cfg.prover_seq_no as u64)
        .await
        .context("Circuit4ShplonkPipeline::prove failed")?;
    info!(
        seq_no = proof.seq_no,
        self_verified = proof.self_verified,
        pi_len = proof.public_instances_hex.len(),
        calldata_bytes = proof.proof_hex.len() / 2,
        "pipeline produced PartnerWithdrawalProof (SHPLONK aggregator calldata)",
    );

    if let Some(out_dir) = cfg.prover_out_dir.as_ref() {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("mkdir prover_out_dir {}", out_dir.display()))?;
        let out_path = out_dir.join(format!("proof_event_{:06}.json", cfg.prover_seq_no));
        let json = serde_json::json!({
            "seq_no": proof.seq_no,
            "proof_hex": proof.proof_hex,
            "public_instances_hex": proof.public_instances_hex,
            "self_verified": proof.self_verified,
        });
        std::fs::write(&out_path, serde_json::to_vec_pretty(&json)?)
            .with_context(|| format!("write {}", out_path.display()))?;
        info!(out = %out_path.display(), "persisted proof_event JSON");
    }

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
