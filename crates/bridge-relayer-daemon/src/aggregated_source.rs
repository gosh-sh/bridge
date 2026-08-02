//! [`BlockSource`] / [`BkUpdateSource`] wrapper that swaps the daemon's
//! Blake2b Circuit 1A/1B/2 proof bytes for **Poseidon** R15 SHPLONK aggregator
//! calldata, produced out-of-process via
//! [`crate::aggregator::Circuit12ShplonkPipeline`].
//!
//! Motivation. The AN-side daemon (`bridge-prover-lib`) proves Circuits 1A/1B/2
//! with a Blake2b Fiat–Shamir transcript — the flavour AN's own
//! `ZKHALO2VERIFYWITHVK` VM opcode expects. The Ethereum-side aggregator only
//! consumes **Poseidon** inner snarks. Rather than re-plumb the daemon to also
//! prove Poseidon (which would double proving cost per bundle and break AN-side
//! self-verify), we shell out to the orchestrator's
//! `export-1a1b2-poseidon-snark` binary to re-prove the same live witness with
//! the ETH-side flavour, then wrap the snark in
//! `bridge-evm-aggregator::aggregate-proof`. This mirrors the pattern already
//! working for Circuit 4 (see [`crate::aggregator::Circuit4ShplonkPipeline`]),
//! and keeps the two toolchains in separate cargo workspaces (the daemon uses
//! the gosh halo2-base fork; the aggregator uses axiom-crypto/halo2-lib — mixing
//! them in one build unit does not compile).
//!
//! Wiring. [`AggregatedBlockSource`] wraps an existing [`Arc<LiveBlockSource>`]:
//!
//! - `fetch(target)` — delegate to the inner [`BlockSource::fetch`]. If a
//!   bundle came back, [`peek_pending_bundle`] the raw
//!   [`BundleProofArtifacts`] (needed for `last_seen_block_seq_no`, which the
//!   shape-preserving [`AnBlockData::from`] conversion drops); snapshot the
//!   driver's [`BridgeState`] to a per-bundle temp JSON so the C2 subprocess
//!   has a `--state` argument free from state.json races (the daemon persists
//!   its state.json only after ack); run the two calldata pipelines; splice
//!   the results into `AnBlockData.attestation_proof` /
//!   `AnBlockData.layer_hashes_proof`. Ack semantics are unchanged — the
//!   inner [`LiveBlockSource`] retains the pending [`BundleProofArtifacts`]
//!   and clears it only when [`ack_last_bundle`] fires after the ETH tx.
//!
//! - `fetch_bk_update(target)` — same shape, minus C2: BK-set rotations only
//!   consume an attestation proof (Solidity `applyBkSetUpdate`). Snapshot is
//!   not needed; the attestation subprocess is stateless.
//!
//! [`peek_pending_bundle`]: crate::live_source::LiveBlockSource::peek_pending_bundle
//! [`BridgeState`]: bridge_prover_lib::bridge_state::BridgeState

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use tracing::info;

use crate::{
    aggregator::{Circuit12ShplonkPipeline, Circuit1a1b2SnarkProver, ProofAggregator},
    error::RelayerError,
    live_source::LiveBlockSource,
    source::{BkUpdateSource, BlockSource},
    types::{AnBlockData, BkSetUpdateData},
};

/// Wraps an [`Arc<LiveBlockSource>`] with Poseidon-side calldata aggregation.
///
/// `snark_dir` is the working directory for per-bundle `.snark` /
/// `.instances.bin` artefacts (created if missing). It is reused across
/// invocations — subsequent runs simply overwrite the fixed-name files
/// (`circuit1a.snark`, `circuit1b.snark`, `circuit2.snark`).
pub struct AggregatedBlockSource<S, A>
where
    S: Circuit1a1b2SnarkProver,
    A: ProofAggregator,
{
    inner: Arc<LiveBlockSource>,
    pipeline: Circuit12ShplonkPipeline<S, A>,
    snark_dir: PathBuf,
}

impl<S, A> AggregatedBlockSource<S, A>
where
    S: Circuit1a1b2SnarkProver,
    A: ProofAggregator,
{
    pub fn new(
        inner: Arc<LiveBlockSource>,
        pipeline: Circuit12ShplonkPipeline<S, A>,
        snark_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            inner,
            pipeline,
            snark_dir: snark_dir.into(),
        }
    }

    /// Access the wrapped [`LiveBlockSource`] — used by `daemon.rs` to keep
    /// ack routing untouched.
    pub fn inner(&self) -> &Arc<LiveBlockSource> {
        &self.inner
    }

    /// Snapshot the driver's current `BridgeState` to a per-bundle temp file
    /// so the C2 subprocess reads a stable view even if a concurrent ack
    /// rewrites the daemon's state.json. Path lifetime is bounded by the
    /// caller's `TempFile` binding — we return an absolute path.
    async fn snapshot_state_to_temp(
        &self,
        seq_no: u64,
    ) -> Result<tempfile::NamedTempFile, RelayerError> {
        let snap = self.inner.driver_snapshot().await;
        let file = tempfile::Builder::new()
            .prefix(&format!("bridge_state_{seq_no}_"))
            .suffix(".json")
            .tempfile()
            .map_err(|e| RelayerError::other(format!("tempfile: {e}")))?;
        let path = file
            .path()
            .to_str()
            .ok_or_else(|| RelayerError::other("tempfile path is not UTF-8"))?
            .to_string();
        snap.save(&path)
            .map_err(|e| RelayerError::other(format!("save snapshot state: {e}")))?;
        Ok(file)
    }
}

#[async_trait]
impl<S, A> BlockSource for AggregatedBlockSource<S, A>
where
    S: Circuit1a1b2SnarkProver + 'static,
    A: ProofAggregator + 'static,
{
    async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let Some(mut block) = self.inner.fetch(target).await? else {
            return Ok(None);
        };
        // Recover the raw pending artifacts to source `last_seen_block_seq_no`
        // — that public input is required by the attestation circuits but
        // does not survive the shape-preserving `AnBlockData::from`.
        let pending = self.inner.peek_pending_bundle().await.ok_or_else(|| {
            RelayerError::other(
                "aggregated_source: fetch returned a block but peek_pending_bundle is empty \
                 (LiveBlockSource contract violation)",
            )
        })?;
        let seqno = pending.block_seq_no;
        let last_seen = u32::try_from(pending.last_seen_block_seq_no).map_err(|_| {
            RelayerError::other(format!(
                "last_seen_block_seq_no {} does not fit u32",
                pending.last_seen_block_seq_no
            ))
        })?;

        info!(
            seq_no = seqno,
            last_seen = last_seen,
            "aggregated_source: re-proving attestation + layer with Poseidon",
        );

        let state_file = self.snapshot_state_to_temp(seqno).await?;

        let attestation_calldata = self
            .pipeline
            .prove_attestation(block.fin_type, seqno, last_seen, &self.snark_dir)
            .await?;
        let layer_calldata = self
            .pipeline
            .prove_layer(seqno, state_file.path(), &self.snark_dir)
            .await?;

        block.attestation_proof = alloy::primitives::Bytes::from(attestation_calldata);
        block.layer_hashes_proof = alloy::primitives::Bytes::from(layer_calldata);
        Ok(Some(block))
    }

    async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.inner.ack_last_bundle(seq_no).await
    }

    async fn driver_snapshot(
        &self,
    ) -> Option<bridge_prover_lib::bridge_state::BridgeState> {
        self.inner.driver_snapshot().await.into()
    }
}

#[async_trait]
impl<S, A> BkUpdateSource for AggregatedBlockSource<S, A>
where
    S: Circuit1a1b2SnarkProver + 'static,
    A: ProofAggregator + 'static,
{
    async fn fetch_bk_update(
        &self,
        target: u64,
    ) -> Result<Option<BkSetUpdateData>, RelayerError> {
        let Some(mut upd) = self.inner.fetch_bk_update(target).await? else {
            return Ok(None);
        };
        let pending = self.inner.peek_pending_bk_update().await.ok_or_else(|| {
            RelayerError::other(
                "aggregated_source: fetch_bk_update returned a block but peek_pending_bk_update \
                 is empty (LiveBlockSource contract violation)",
            )
        })?;
        let seqno = pending.block_seq_no;
        let last_seen = u32::try_from(pending.last_seen_bk_update_seq_no).map_err(|_| {
            RelayerError::other(format!(
                "last_seen_bk_update_seq_no {} does not fit u32",
                pending.last_seen_bk_update_seq_no
            ))
        })?;

        info!(
            seq_no = seqno,
            last_seen = last_seen,
            "aggregated_source: re-proving bk-update attestation with Poseidon",
        );

        let attestation_calldata = self
            .pipeline
            .prove_attestation(upd.fin_type, seqno, last_seen, &self.snark_dir)
            .await?;
        upd.attestation_proof = alloy::primitives::Bytes::from(attestation_calldata);
        Ok(Some(upd))
    }

    async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.inner.ack_last_bk_update(seq_no).await
    }
}
