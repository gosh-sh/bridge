//! [`BlockSource`] / [`BkUpdateSource`] wrapper that swaps the daemon's
//! Circuit 1A/1B/2 Poseidon proof bytes for **R15 SHPLONK aggregator
//! calldata**, produced by [`crate::aggregator::Circuit12ShplonkPipeline`]
//! (in-process snark wrap + `aggregate-proof` subprocess).
//!
//! Motivation. `bridge-prover-lib`'s [`LiveProverDriver`] proves Circuits
//! 1A/1B/2 with whichever transcript the caller selects via
//! [`LiveProverConfig::transcript`]; the relayer's `run_daemon_live` requests
//! [`TranscriptKind::Poseidon`] so the delivered [`BundleProofArtifacts`]
//! already carry Poseidon-transcript proof bytes — exactly the flavour the
//! ETH aggregator consumes. All this wrapper does is take those bytes plus
//! the peek fields, wrap them into a snark-verifier `Snark` **in-process**
//! (via `bridge-snark-wrap`), and hand the bincode Snark to
//! `bridge-evm-aggregator::aggregate-proof`.
//!
//! What this replaces. The former design shelled out to
//! `bridge-snark-utils/export-1a1b2-poseidon-snark`, which
//! independently re-fetched from GraphQL, re-ran `real_chain_builder`, and
//! re-proved the same witness — doubling the GQL fetches, chain builds, and
//! Halo2 proves per key block. The subprocess is gone; only the
//! `aggregate-proof` subprocess remains (its `snark-verifier` transitive
//! graph resolves halo2-base to axiom's crate, which cannot coexist in one
//! build unit with the gosh fork the daemon links).
//!
//! Wiring. [`AggregatedBlockSource`] wraps an existing
//! [`Arc<LiveBlockSource>`]:
//!
//! - `fetch(target)` — delegate to the inner [`BlockSource::fetch`]. If a
//!   bundle came back, [`peek_pending_bundle`] the raw [`BundleProofArtifacts`]
//!   (source of the Poseidon proof bytes + the public-input fields the
//!   shape-preserving [`AnBlockData::from`] conversion drops); run the
//!   wrap+aggregate pipeline for the attestation proof and again for the layer
//!   proof; splice the resulting calldata into `AnBlockData.attestation_proof`
//!   / `AnBlockData.layer_hashes_proof`. Ack semantics are unchanged — the
//!   inner [`LiveBlockSource`] retains the pending [`BundleProofArtifacts`] and
//!   clears it only when [`ack_last_bundle`] fires after the ETH tx.
//!
//! - `fetch_bk_update(target)` — same shape, minus C2: BK-set rotations only
//!   consume an attestation proof (Solidity `applyBkSetUpdate`).
//!
//! [`peek_pending_bundle`]: crate::live_source::LiveBlockSource::peek_pending_bundle
//! [`LiveProverDriver`]: bridge_prover_lib::live_driver::LiveProverDriver
//! [`LiveProverConfig::transcript`]: bridge_prover_lib::live_driver::LiveProverConfig
//! [`TranscriptKind::Poseidon`]: bridge_prover_lib::transcript::TranscriptKind

use std::sync::Arc;

use async_trait::async_trait;
use bridge_prover_lib::{
    bridge_state::BridgeState,
    live_driver::{BkUpdateProofArtifacts, BundleProofArtifacts},
};
use tracing::info;

use crate::{
    aggregator::{Circuit12ShplonkPipeline, ProofAggregator, SnarkWrapper},
    error::RelayerError,
    live_source::LiveBlockSource,
    source::{BkUpdateSource, BlockSource},
    types::{AnBlockData, BkSetUpdateData},
};

/// The `LiveBlockSource`-specific behaviour [`AggregatedBlockSource`] needs
/// beyond [`BlockSource`] + [`BkUpdateSource`]: peek the pending
/// prover-lib artifacts (for `last_seen_*` public inputs the shape-preserving
/// `AnBlockData::from` drops) + snapshot the driver's `BridgeState` (fed to
/// the C2 subprocess via `--state`).
///
/// Extracted from `LiveBlockSource` so the wrapper can be integration-tested
/// with a stub source that doesn't need a real GQL client / driver.
#[async_trait]
pub trait LiveArtifactPeek: Send + Sync {
    async fn peek_pending_bundle(&self) -> Option<BundleProofArtifacts>;
    async fn peek_pending_bk_update(&self) -> Option<BkUpdateProofArtifacts>;
    async fn snapshot_bridge_state(&self) -> BridgeState;
}

#[async_trait]
impl LiveArtifactPeek for LiveBlockSource {
    async fn peek_pending_bundle(&self) -> Option<BundleProofArtifacts> {
        LiveBlockSource::peek_pending_bundle(self).await
    }
    async fn peek_pending_bk_update(&self) -> Option<BkUpdateProofArtifacts> {
        LiveBlockSource::peek_pending_bk_update(self).await
    }
    async fn snapshot_bridge_state(&self) -> BridgeState {
        // `LiveBlockSource::driver_snapshot` is inherent (returns
        // `BridgeState` directly, not the `Option`-wrapping trait method).
        LiveBlockSource::driver_snapshot(self).await
    }
}

/// Wraps a live source with Poseidon-side calldata aggregation. Generic over
/// the inner source so integration tests can plug in a stub implementing
/// [`BlockSource`] + [`BkUpdateSource`] + [`LiveArtifactPeek`] without
/// standing up a real `LiveProverDriver`.
pub struct AggregatedBlockSource<I, W, A>
where
    I: BlockSource + BkUpdateSource + LiveArtifactPeek,
    W: SnarkWrapper,
    A: ProofAggregator,
{
    inner: Arc<I>,
    pipeline: Circuit12ShplonkPipeline<W, A>,
}

impl<I, W, A> AggregatedBlockSource<I, W, A>
where
    I: BlockSource + BkUpdateSource + LiveArtifactPeek,
    W: SnarkWrapper,
    A: ProofAggregator,
{
    pub fn new(inner: Arc<I>, pipeline: Circuit12ShplonkPipeline<W, A>) -> Self {
        Self {
            inner,
            pipeline,
        }
    }

    /// Access the wrapped inner source — used by `daemon.rs` to keep
    /// ack routing untouched.
    pub fn inner(&self) -> &Arc<I> {
        &self.inner
    }
}

#[async_trait]
impl<I, W, A> BlockSource for AggregatedBlockSource<I, W, A>
where
    I: BlockSource + BkUpdateSource + LiveArtifactPeek + 'static,
    W: SnarkWrapper + 'static,
    A: ProofAggregator + 'static,
{
    async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let Some(mut block) = self.inner.fetch(target).await? else {
            return Ok(None);
        };
        // Recover the raw pending artifacts. Beyond `last_seen_block_seq_no`
        // (dropped by the shape-preserving `AnBlockData::from`), we also need
        // the Poseidon proof bytes and the byte-form public inputs to
        // reconstruct the Fr instance vectors the aggregator wrap consumes.
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
            "aggregated_source: wrap+aggregate attestation + layer (Poseidon)",
        );

        let t_bundle = std::time::Instant::now();
        let attestation_calldata = self
            .pipeline
            .aggregate_attestation(
                block.fin_type,
                &pending.attestation_proof,
                &pending.block_id_be,
                &pending.bk_set_commitment_be,
                seqno,
                last_seen,
            )
            .await?;
        let layer_calldata = self
            .pipeline
            .aggregate_layer(
                &pending.layer_hashes_proof,
                &pending.block_id_be,
                &pending.bk_set_commitment_be,
                pending.num_layers,
                &pending.layer_hashes_be,
                &pending.prev_max_level_layer_hash_be,
            )
            .await?;
        info!(
            seq_no = seqno,
            "aggregated_source: bundle wrap+aggregate (attestation + layer) total {} ms",
            t_bundle.elapsed().as_millis(),
        );

        block.attestation_proof = alloy::primitives::Bytes::from(attestation_calldata);
        block.layer_hashes_proof = alloy::primitives::Bytes::from(layer_calldata);
        Ok(Some(block))
    }

    async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.inner.ack_last_bundle(seq_no).await
    }

    async fn driver_snapshot(&self) -> Option<bridge_prover_lib::bridge_state::BridgeState> {
        self.inner.driver_snapshot().await
    }
}

#[async_trait]
impl<I, W, A> BkUpdateSource for AggregatedBlockSource<I, W, A>
where
    I: BlockSource + BkUpdateSource + LiveArtifactPeek + 'static,
    W: SnarkWrapper + 'static,
    A: ProofAggregator + 'static,
{
    async fn fetch_bk_update(&self, target: u64) -> Result<Option<BkSetUpdateData>, RelayerError> {
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
                "attestation lastSeen (layer cursor) {} does not fit u32",
                pending.last_seen_bk_update_seq_no
            ))
        })?;

        info!(
            seq_no = seqno,
            last_seen = last_seen,
            "aggregated_source: wrap+aggregate bk-update attestation (Poseidon)",
        );

        // For bk-update, the attestation is against the OLD BK-set commitment
        // (Circuit 1A/1B binds `bk_set_commitment_fr` = old set); the layer
        // proof is not part of this lane.
        let attestation_calldata = self
            .pipeline
            .aggregate_attestation(
                upd.fin_type,
                &pending.attestation_proof,
                &pending.block_id_be,
                &pending.old_bk_set_commitment_be,
                seqno,
                last_seen,
            )
            .await?;
        upd.attestation_proof = alloy::primitives::Bytes::from(attestation_calldata);
        Ok(Some(upd))
    }

    async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
        self.inner.ack_last_bk_update(seq_no).await
    }
}

#[cfg(test)]
mod tests {
    //! Integration coverage for the peek → wrap → aggregate → swap path —
    //! proves the wiring between [`BlockSource`] / [`BkUpdateSource`]
    //! delegation, [`LiveArtifactPeek`] extraction, and
    //! [`Circuit12ShplonkPipeline`] stays intact if any of the three arms
    //! change. Uses a stubbed inner source + [`MockSnarkWrapper`] +
    //! [`RecordingAggregator`] wrapper around [`MockAggregator`] so no real
    //! halo2 / subprocess plumbing runs.
    //!
    //! What is NOT covered here: the real snark-verifier wrap (which needs
    //! params_dir + VK + SRS + config on disk; validated at the
    //! `bridge-snark-wrap` crate level), the `aggregate-proof` subprocess
    //! itself, the `LiveBlockSource` GQL/driver plumbing (covered by
    //! prover-lib integration tests), and end-to-end Solidity calldata
    //! acceptance (covered by anvil harness).
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use alloy::primitives::{Bytes, U256};
    use bridge_prover_lib::{
        bridge_state::BridgeState,
        live_driver::{BkUpdateProofArtifacts, BundleFinalizationType, BundleProofArtifacts},
        transcript::TranscriptKind,
    };

    use super::*;
    use crate::{
        aggregator::{MockAggregator, MockSnarkWrapper, ProofAggregator},
        types::{AnBlockData, BkSetUpdateData, FinalizationType, MAX_LAYER_HASHES},
    };

    /// Records `aggregate` invocations so tests can assert the pipeline
    /// received the expected `(verifier_name, snark_path)` after passing
    /// through [`AggregatedBlockSource`]. Wraps [`MockAggregator`] so the
    /// downstream `BlockSource` calldata swap still sees valid bytes.
    #[derive(Default)]
    struct RecordingAggregator {
        inner: MockAggregator,
        calls: Mutex<Vec<(String, PathBuf)>>,
    }

    #[async_trait]
    impl ProofAggregator for RecordingAggregator {
        async fn aggregate(
            &self,
            inner_snark: &Path,
            verifier_name: &str,
        ) -> Result<Vec<u8>, RelayerError> {
            self.calls
                .lock()
                .unwrap()
                .push((verifier_name.to_string(), inner_snark.to_path_buf()));
            self.inner.aggregate(inner_snark, verifier_name).await
        }
    }

    /// Minimal in-memory source implementing every trait
    /// [`AggregatedBlockSource`] requires of its inner:
    /// [`BlockSource`] + [`BkUpdateSource`] + [`LiveArtifactPeek`].
    ///
    /// Blocks and bk-updates are staged as `(payload, matching artifacts)`
    /// pairs — `fetch` returns the payload and `peek_pending_*` returns
    /// the artifact, mirroring `LiveBlockSource`'s "fetch stashes, peek
    /// reads, ack clears" contract.
    #[derive(Default)]
    struct StubInnerSource {
        block: Mutex<Option<AnBlockData>>,
        bundle_pending: Mutex<Option<BundleProofArtifacts>>,
        bkupd: Mutex<Option<BkSetUpdateData>>,
        bkupd_pending: Mutex<Option<BkUpdateProofArtifacts>>,
        acked_bundles: Mutex<Vec<u64>>,
        acked_bkupds: Mutex<Vec<u64>>,
    }

    impl StubInnerSource {
        fn stage_bundle(&self, block: AnBlockData, art: BundleProofArtifacts) {
            *self.block.lock().unwrap() = Some(block);
            *self.bundle_pending.lock().unwrap() = Some(art);
        }
        fn stage_bkupd(&self, upd: BkSetUpdateData, art: BkUpdateProofArtifacts) {
            *self.bkupd.lock().unwrap() = Some(upd);
            *self.bkupd_pending.lock().unwrap() = Some(art);
        }
        /// Stage a block WITHOUT pending artifacts — used to exercise the
        /// "LiveBlockSource contract violation" branch in
        /// `AggregatedBlockSource::fetch`.
        fn stage_bundle_without_pending(&self, block: AnBlockData) {
            *self.block.lock().unwrap() = Some(block);
            *self.bundle_pending.lock().unwrap() = None;
        }
    }

    #[async_trait]
    impl BlockSource for StubInnerSource {
        async fn fetch(&self, _target: u64) -> Result<Option<AnBlockData>, RelayerError> {
            Ok(self.block.lock().unwrap().clone())
        }
        async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
            self.acked_bundles.lock().unwrap().push(seq_no);
            Ok(())
        }
    }

    #[async_trait]
    impl BkUpdateSource for StubInnerSource {
        async fn fetch_bk_update(
            &self,
            _target: u64,
        ) -> Result<Option<BkSetUpdateData>, RelayerError> {
            Ok(self.bkupd.lock().unwrap().clone())
        }
        async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> {
            self.acked_bkupds.lock().unwrap().push(seq_no);
            Ok(())
        }
    }

    #[async_trait]
    impl LiveArtifactPeek for StubInnerSource {
        async fn peek_pending_bundle(&self) -> Option<BundleProofArtifacts> {
            self.bundle_pending.lock().unwrap().clone()
        }
        async fn peek_pending_bk_update(&self) -> Option<BkUpdateProofArtifacts> {
            self.bkupd_pending.lock().unwrap().clone()
        }
        async fn snapshot_bridge_state(&self) -> BridgeState {
            BridgeState::new(4)
        }
    }

    fn stub_block(seqno: u64, fin: FinalizationType) -> AnBlockData {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(0x1111u64);
        AnBlockData {
            fin_type: fin,
            block_id: U256::from(seqno),
            bk_set_commitment: U256::from(0xBE5E7u64),
            block_seq_no: seqno,
            num_layers: 1,
            layer_hashes,
            prev_max_level_layer_hash: U256::ZERO,
            // Sentinels — after `fetch()` these must be overwritten with
            // the pipeline's calldata.
            attestation_proof: Bytes::from(vec![0xAAu8; 8]),
            layer_hashes_proof: Bytes::from(vec![0xBBu8; 8]),
        }
    }

    fn stub_bundle_artifacts(
        seqno: u64,
        last_seen: u64,
        fin: BundleFinalizationType,
    ) -> BundleProofArtifacts {
        BundleProofArtifacts {
            block_seq_no: seqno,
            block_height: seqno,
            last_seen_block_seq_no: last_seen,
            block_id_be: [0u8; 32],
            fin_type: fin,
            bk_set_commitment_be: [0u8; 32],
            num_layers: 1,
            layer_hashes_be: [[0u8; 32]; bridge_prover_lib::bridge_state::MAX_LAYERS],
            prev_max_level_layer_hash_be: [0u8; 32],
            transcript_kind: TranscriptKind::Blake2b,
            attestation_proof: vec![],
            layer_hashes_proof: vec![],
            attestation_proof_gen_ms: 0,
            layer_proof_gen_ms: 0,
            state_layer_hashes: vec![],
        }
    }

    fn stub_bkupd_data(seqno: u64, fin: FinalizationType) -> BkSetUpdateData {
        BkSetUpdateData {
            fin_type: fin,
            block_id: U256::from(seqno),
            block_seq_no: seqno,
            old_commitment_l2: U256::ZERO,
            new_commitment_l3: U256::from(0xC0FFEEu64),
            sibling_h01: [0u8; 32],
            sibling_h4_7: [0u8; 32],
            sibling_h8_15: [0u8; 32],
            attestation_proof: Bytes::from(vec![0xAAu8; 8]),
        }
    }

    fn stub_bkupd_artifacts(
        seqno: u64,
        last_seen: u64,
        fin: BundleFinalizationType,
    ) -> BkUpdateProofArtifacts {
        BkUpdateProofArtifacts {
            block_seq_no: seqno,
            block_height: seqno,
            last_seen_bk_update_seq_no: last_seen,
            block_id_be: [0u8; 32],
            fin_type: fin,
            old_bk_set_commitment_be: [0u8; 32],
            new_bk_set_commitment_be: [0u8; 32],
            merkle_sibling_h01_be: [0u8; 32],
            merkle_sibling_h4_7_be: [0u8; 32],
            merkle_sibling_h8_15_be: [0u8; 32],
            transcript_kind: TranscriptKind::Blake2b,
            attestation_proof: vec![],
            new_pubkeys: HashMap::new(),
            attestation_proof_gen_ms: 0,
        }
    }

    fn make_aggregated(
        stub: Arc<StubInnerSource>,
    ) -> AggregatedBlockSource<StubInnerSource, MockSnarkWrapper, RecordingAggregator> {
        let pipeline = crate::aggregator::Circuit12ShplonkPipeline::new(
            MockSnarkWrapper::default(),
            RecordingAggregator::default(),
        );
        AggregatedBlockSource::new(stub, pipeline)
    }

    #[tokio::test]
    async fn fetch_returns_none_when_inner_returns_none() {
        let stub = Arc::new(StubInnerSource::default());
        let src = make_aggregated(stub);
        assert!(src.fetch(1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fetch_errors_when_inner_returns_block_but_peek_is_empty() {
        // LiveBlockSource contract: `fetch` returning `Some` MUST have
        // stashed a matching pending bundle. `AggregatedBlockSource` maps
        // any violation to a hard error so the daemon does not submit a
        // wrongly-labeled proof.
        let stub = Arc::new(StubInnerSource::default());
        stub.stage_bundle_without_pending(stub_block(42, FinalizationType::Primary));
        let src = make_aggregated(stub);
        let err = src.fetch(42).await.unwrap_err();
        assert!(
            format!("{err}").contains("peek_pending_bundle is empty"),
            "unexpected err: {err}"
        );
    }

    #[tokio::test]
    async fn fetch_runs_pipeline_and_swaps_proofs() {
        let stub = Arc::new(StubInnerSource::default());
        stub.stage_bundle(
            stub_block(100, FinalizationType::Primary),
            stub_bundle_artifacts(100, 99, BundleFinalizationType::Primary),
        );
        let src = make_aggregated(Arc::clone(&stub));

        let out = src
            .fetch(100)
            .await
            .unwrap()
            .expect("fetch must return Some");

        // MockAggregator returns 3616 bytes; sentinel proofs were 8 bytes.
        assert_eq!(out.attestation_proof.len(), 3616);
        assert_eq!(out.layer_hashes_proof.len(), 3616);
        // Identity fields must survive unmodified.
        assert_eq!(out.block_seq_no, 100);
        assert_eq!(out.fin_type, FinalizationType::Primary);

        // Two aggregate() calls: attestation (Primary verifier) then layer.
        let calls = src.pipeline.aggregator.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, crate::aggregator::PRIMARY_VERIFIER_NAME);
        assert_eq!(calls[1].0, crate::aggregator::LAYER_HASHES_VERIFIER_NAME);
    }

    #[tokio::test]
    async fn fetch_routes_fallback_to_fallback_verifier() {
        // `AggregatedBlockSource::fetch` reads `block.fin_type` to pick the
        // aggregator verifier — verifying the mapping here catches accidental
        // hardcodes to Primary.
        let stub = Arc::new(StubInnerSource::default());
        stub.stage_bundle(
            stub_block(200, FinalizationType::Fallback),
            stub_bundle_artifacts(200, 150, BundleFinalizationType::Fallback),
        );
        let src = make_aggregated(Arc::clone(&stub));

        let _ = src.fetch(200).await.unwrap().unwrap();
        let calls = src.pipeline.aggregator.calls.lock().unwrap().clone();
        assert_eq!(calls[0].0, crate::aggregator::FALLBACK_VERIFIER_NAME);
    }

    #[tokio::test]
    async fn fetch_bk_update_swaps_attestation_proof() {
        let stub = Arc::new(StubInnerSource::default());
        stub.stage_bkupd(
            stub_bkupd_data(300, FinalizationType::Primary),
            stub_bkupd_artifacts(300, 299, BundleFinalizationType::Primary),
        );
        let src = make_aggregated(Arc::clone(&stub));

        let out = src
            .fetch_bk_update(300)
            .await
            .unwrap()
            .expect("fetch_bk_update must return Some");
        assert_eq!(out.attestation_proof.len(), 3616);
        assert_eq!(out.block_seq_no, 300);
        assert_eq!(out.fin_type, FinalizationType::Primary);

        // Exactly one aggregate() call — the bk-update lane never runs Circuit 2.
        let calls = src.pipeline.aggregator.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, crate::aggregator::PRIMARY_VERIFIER_NAME);
    }

    #[tokio::test]
    async fn fetch_bk_update_returns_none_when_inner_returns_none() {
        let stub = Arc::new(StubInnerSource::default());
        let src = make_aggregated(stub);
        assert!(src.fetch_bk_update(1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ack_delegates_to_inner() {
        // Ack semantics MUST bypass the aggregator entirely — the raw
        // pending BundleProofArtifacts still lives in `LiveBlockSource`
        // and only its ack clears it.
        let stub = Arc::new(StubInnerSource::default());
        let src = make_aggregated(Arc::clone(&stub));

        src.ack_last_bundle(7).await.unwrap();
        src.ack_last_bk_update(42).await.unwrap();
        assert_eq!(&*stub.acked_bundles.lock().unwrap(), &[7]);
        assert_eq!(&*stub.acked_bkupds.lock().unwrap(), &[42]);
    }
}
