//! [`LiveBlockSource`] — a [`BlockSource`] backed by a live AN node and a
//! live ZK prover orchestrator. Phase 5.2 of the integration plan.
//!
//! The live source is intentionally split into two narrow traits so a
//! deployment can swap either side without rebuilding the relayer:
//!
//! - [`RawBlockProvider`] — "give me the witness materials for block seq N
//!   (BOC, BLS attestation, BK-set snapshot, layer hashes, chain anchor)".
//! - [`BoundProofGenerator`] — "given those witnesses, produce a cross-circuit-
//!   bound `(attestation_proof, layer_hashes_proof)` in gnark Groth16
//!   marshal-solidity bytes".
//!
//! `LiveBlockSource` orchestrates both behind `BlockSource::fetch`:
//!
//! 1. `RawBlockProvider::fetch_raw(target_seq_no)` → `RawBlockWitness` (cheap;
//!    just HTTP / GraphQL against the partner node).
//! 2. `BoundProofGenerator::prove(raw)` → `BoundProofArtifacts` (expensive;
//!    Halo2 SHPLONK on K=20 + gnark wrap; minutes per call).
//! 3. Assemble [`AnBlockData`] from both, validate shape, return.
//!
//! Where the live backends live (planned, not yet wired here):
//!
//! - **`RawBlockProvider`**: thin wrapper around the partner's
//!   `circuit-data-exporter` (sibling repo `acki-nacki`,
//!   branch `bridge_halo2_tests`, path `helpers/circuit_data_exporter`)
//!   plus the [`BkSetClient`](crate::types) we already use for BK metadata.
//!   Pending public GraphQL exposure (port 8600 on the test node is REST-only
//!   as of 2026-05-26 — see `AGENTS.md`). For local 5-node clusters built
//!   with the `history_proofs` feature, the GraphQL endpoint is
//!   `http://127.0.0.1:11000/graphql` (node0).
//! - **`BoundProofGenerator`**: wraps `bridge-prover-orchestrator`'s
//!   `generate_*_proof` + `bridge_prover_lib::compose_layer_hashes_input`
//!   functions and the `gnark-wrappers/circuit-1a` + `circuit-2` subprocess
//!   invocations. The wrap is heavyweight (≈26 min per layer-hashes proof at
//!   production K=19; deposit-proof side is comparable) so prod deployments run
//!   a separate `bridge-prover-daemon` and the relayer talks to it over an
//!   internal queue (Phase 6).
//!
//! Both traits are async + `Send + Sync` so the relayer loop stays
//! single-threaded but a multi-block pipeline can run multiple
//! `BoundProofGenerator::prove` calls in parallel on a pool of worker
//! processes.

use alloy::primitives::{Bytes, U256};
use async_trait::async_trait;

use crate::{
    error::RelayerError,
    source::BlockSource,
    types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES},
};

/// Raw witness materials for one AN block, as the partner node sees them.
///
/// This is the same data shape `circuit-data-exporter` writes to its
/// `circuit_test_data_*.json` fixtures (see `AGENTS.md`), promoted from
/// the JSON-keyed view to a strongly-typed Rust struct. Field names match
/// the Halo2 circuit's witness naming so the boundary is verbatim.
#[derive(Clone, Debug)]
pub struct RawBlockWitness {
    /// Finalisation path the node finalised this block on.
    pub fin_type: FinalizationType,
    /// 32-byte AN block identifier (Merkle root of the block envelope).
    pub block_id: U256,
    /// Poseidon commitment to the active BK set (matches
    /// [`crate::bk_set_sentry::BkSetPoller`]'s view).
    pub bk_set_commitment: U256,
    /// AN block sequence number.
    pub block_seq_no: u64,
    /// Number of active layer slots in this block (1..=`MAX_LAYER_HASHES`).
    pub num_layers: u8,
    /// Per-layer Poseidon roots (tail beyond `num_layers` must be zero).
    pub layer_hashes: [U256; MAX_LAYER_HASHES],
    /// Anchor for the previous block's top layer (matches the bridge's
    /// `storedPrevMaxLevelLayerHash`).
    pub prev_max_level_layer_hash: U256,
    /// Encoded witness payload the prover needs (BOC bytes, BLS
    /// signatures, signer-index map, chain-of-dense-proofs siblings).
    /// Opaque from the relayer's POV — only the prover unpacks it.
    pub prover_witness: Vec<u8>,
}

impl RawBlockWitness {
    /// Validate the shape the bridge contract enforces. Cheap; runs before
    /// we hand the witness off to the (expensive) prover.
    pub fn validate_shape(&self) -> Result<(), RelayerError> {
        if self.num_layers == 0 || self.num_layers as usize > MAX_LAYER_HASHES {
            return Err(RelayerError::other(format!(
                "RawBlockWitness: num_layers {} out of 1..=10",
                self.num_layers
            )));
        }
        for i in self.num_layers as usize..MAX_LAYER_HASHES {
            if !self.layer_hashes[i].is_zero() {
                return Err(RelayerError::other(format!(
                    "RawBlockWitness: layer_hashes[{i}] must be zero (tail past num_layers)"
                )));
            }
        }
        Ok(())
    }
}

/// `BoundProofGenerator` output — two gnark Groth16 proofs whose public
/// inputs are cross-circuit-bound (same `block_id`, same `bk_set_poseidon`,
/// same `block_seq_no`) by construction.
///
/// Both are 256-byte marshal-solidity blobs (matches the
/// `AckiNackiBridge.verifyBlock` calldata layout).
#[derive(Clone, Debug)]
pub struct BoundProofArtifacts {
    pub attestation_proof: Bytes,
    pub layer_hashes_proof: Bytes,
}

/// Async fetch of the raw AN block witness for a given sequence number.
///
/// Returns:
/// - `Ok(Some(witness))` — block is finalised and ready to prove.
/// - `Ok(None)`          — block isn't finalised yet (or witness assembly is
///   gated waiting on a downstream sibling — e.g. the Circuit-2 dense-chain
///   extension hasn't caught up). The relayer waits and retries.
/// - `Err(RelayerError)` — terminal upstream failure (HTTP / schema / malformed
///   payload).
#[async_trait]
pub trait RawBlockProvider: Send + Sync {
    async fn fetch_raw(&self, target_seq_no: u64) -> Result<Option<RawBlockWitness>, RelayerError>;
}

/// Generate the cross-circuit-bound Groth16 wrap from the raw witness.
///
/// This is where the heavy lift lives — Halo2 SHPLONK + gnark wrap, in the
/// order of minutes per call at production sizes. The relayer always awaits
/// the future, so the implementation is free to fan out to a worker pool
/// or to an internal queue (Phase 6 `bridge-prover-daemon`).
#[async_trait]
pub trait BoundProofGenerator: Send + Sync {
    async fn prove(&self, raw: &RawBlockWitness) -> Result<BoundProofArtifacts, RelayerError>;
}

/// [`BlockSource`] composed of a [`RawBlockProvider`] +
/// [`BoundProofGenerator`].
///
/// Pluggable both ways: swap the raw-fetch side to retarget a different
/// AN node deployment, or swap the prover side to point at a daemon
/// instead of in-process Halo2.
pub struct LiveBlockSource<P, G>
where
    P: RawBlockProvider,
    G: BoundProofGenerator,
{
    raw: P,
    prover: G,
}

impl<P, G> LiveBlockSource<P, G>
where
    P: RawBlockProvider,
    G: BoundProofGenerator,
{
    pub fn new(raw: P, prover: G) -> Self {
        Self {
            raw,
            prover,
        }
    }

    /// Access the underlying raw provider (e.g. for health checks).
    pub fn raw_provider(&self) -> &P {
        &self.raw
    }

    /// Access the underlying prover (e.g. for warm-up calls).
    pub fn prover(&self) -> &G {
        &self.prover
    }
}

#[async_trait]
impl<P, G> BlockSource for LiveBlockSource<P, G>
where
    P: RawBlockProvider + 'static,
    G: BoundProofGenerator + 'static,
{
    async fn fetch(&self, target_seq_no: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let raw = match self.raw.fetch_raw(target_seq_no).await? {
            Some(r) => r,
            None => return Ok(None),
        };

        if raw.block_seq_no != target_seq_no {
            return Err(RelayerError::SeqNoMismatch {
                requested: target_seq_no,
                got: raw.block_seq_no,
            });
        }
        raw.validate_shape()?;

        let proofs = self.prover.prove(&raw).await?;
        crate::proof_validation::validate_attestation_proof(raw.fin_type, &proofs.attestation_proof)?;
        crate::proof_validation::validate_layer_hashes_proof(&proofs.layer_hashes_proof)?;

        let block = AnBlockData {
            fin_type: raw.fin_type,
            block_id: raw.block_id,
            bk_set_commitment: raw.bk_set_commitment,
            block_seq_no: raw.block_seq_no,
            num_layers: raw.num_layers,
            layer_hashes: raw.layer_hashes,
            prev_max_level_layer_hash: raw.prev_max_level_layer_hash,
            attestation_proof: proofs.attestation_proof,
            layer_hashes_proof: proofs.layer_hashes_proof,
        };
        block.validate_shape()?;
        Ok(Some(block))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test plumbing — mocks for both halves
// ─────────────────────────────────────────────────────────────────────

/// Pre-baked raw witnesses indexed by `block_seq_no` (interior mutability so
/// tests can stage / mutate at runtime). Mirrors
/// [`crate::source::InMemoryBlockSource`]'s shape but for the *raw* side.
pub struct InMemoryRawBlockProvider {
    blocks: std::sync::Mutex<std::collections::BTreeMap<u64, RawBlockWitness>>,
}

impl InMemoryRawBlockProvider {
    pub fn new() -> Self {
        Self {
            blocks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    pub fn insert(&self, raw: RawBlockWitness) {
        self.blocks
            .lock()
            .expect("poisoned lock")
            .insert(raw.block_seq_no, raw);
    }
}

impl Default for InMemoryRawBlockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RawBlockProvider for InMemoryRawBlockProvider {
    async fn fetch_raw(&self, target_seq_no: u64) -> Result<Option<RawBlockWitness>, RelayerError> {
        Ok(self
            .blocks
            .lock()
            .expect("poisoned lock")
            .get(&target_seq_no)
            .cloned())
    }
}

/// Deterministic mock prover — returns the configured proof bytes verbatim,
/// regardless of which witness it's handed. Used for unit tests so the
/// expensive Halo2+gnark pipeline doesn't have to run in CI.
pub struct StubBoundProofGenerator {
    attestation_proof: Bytes,
    layer_hashes_proof: Bytes,
}

impl StubBoundProofGenerator {
    /// Build a stub that returns 256-byte all-`0xAB` and all-`0xCD` blobs
    /// for attestation / layer-hashes respectively. The bridge mock
    /// verifiers in this crate accept any 256-byte payload so the round
    /// trip passes shape validation.
    pub fn deterministic() -> Self {
        Self {
            attestation_proof: Bytes::from(vec![0xABu8; 256]),
            layer_hashes_proof: Bytes::from(vec![0xCDu8; 256]),
        }
    }

    pub fn with_bytes(attestation_proof: Bytes, layer_hashes_proof: Bytes) -> Self {
        Self {
            attestation_proof,
            layer_hashes_proof,
        }
    }
}

#[async_trait]
impl BoundProofGenerator for StubBoundProofGenerator {
    async fn prove(&self, _raw: &RawBlockWitness) -> Result<BoundProofArtifacts, RelayerError> {
        Ok(BoundProofArtifacts {
            attestation_proof: self.attestation_proof.clone(),
            layer_hashes_proof: self.layer_hashes_proof.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_block(seq: u64) -> RawBlockWitness {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(seq * 7 + 1);
        layer_hashes[1] = U256::from(seq * 7 + 2);
        RawBlockWitness {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            bk_set_commitment: U256::from(0xBE5E7u64),
            block_seq_no: seq,
            num_layers: 2,
            layer_hashes,
            prev_max_level_layer_hash: U256::ZERO,
            prover_witness: vec![0u8; 4],
        }
    }

    #[tokio::test]
    async fn live_source_round_trip() {
        let raw = InMemoryRawBlockProvider::new();
        raw.insert(raw_block(7));
        let prover = StubBoundProofGenerator::deterministic();
        let src = LiveBlockSource::new(raw, prover);

        assert!(src.fetch(99).await.unwrap().is_none(), "missing seq → None");

        let block = src
            .fetch(7)
            .await
            .unwrap()
            .expect("seed block should resolve");
        assert_eq!(block.block_seq_no, 7);
        assert_eq!(block.num_layers, 2);
        assert_eq!(block.layer_hashes[0], U256::from(7 * 7 + 1));
        assert_eq!(block.attestation_proof.len(), 256);
        assert_eq!(block.layer_hashes_proof.len(), 256);
        block.validate_shape().expect("shape valid");
    }

    #[tokio::test]
    async fn live_source_propagates_raw_seqno_mismatch() {
        struct Bogus;
        #[async_trait]
        impl RawBlockProvider for Bogus {
            async fn fetch_raw(
                &self,
                _target: u64,
            ) -> Result<Option<RawBlockWitness>, RelayerError> {
                Ok(Some(raw_block(42)))
            }
        }
        let prover = StubBoundProofGenerator::deterministic();
        let src = LiveBlockSource::new(Bogus, prover);
        let err = src.fetch(7).await.expect_err("seq mismatch");
        match err {
            RelayerError::SeqNoMismatch {
                requested,
                got,
            } => {
                assert_eq!(requested, 7);
                assert_eq!(got, 42);
            },
            other => panic!("expected SeqNoMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn live_source_rejects_bad_proof_length() {
        let raw = InMemoryRawBlockProvider::new();
        raw.insert(raw_block(1));
        let prover = StubBoundProofGenerator::with_bytes(
            Bytes::from(vec![0xAA; 100]), // too short for primary SHPLONK or Groth16
            Bytes::from(vec![0xBB; 256]),
        );
        let src = LiveBlockSource::new(raw, prover);
        let err = src.fetch(1).await.expect_err("bad proof length");
        match err {
            RelayerError::Other(msg) => assert!(
                msg.contains("attestation proof too short"),
                "unexpected: {msg}"
            ),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn live_source_propagates_raw_shape_error() {
        // num_layers = 0 violates the contract's shape.
        let raw = InMemoryRawBlockProvider::new();
        let mut bad = raw_block(3);
        bad.num_layers = 0;
        raw.insert(bad);
        let prover = StubBoundProofGenerator::deterministic();
        let src = LiveBlockSource::new(raw, prover);
        let err = src.fetch(3).await.expect_err("bad shape");
        match err {
            RelayerError::Other(msg) => {
                assert!(msg.contains("num_layers 0"), "unexpected: {msg}")
            },
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
