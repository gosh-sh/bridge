//! Cross-thread event-proof bundle types.
//!
//! A cross-thread `WithdrawalInitiated` claim splits into a `final` Circuit 4
//! proof (same shape as today's same-thread proof, published against the
//! event's own thread anchor) plus 1..=`N_BUNDLE_MAX` `BridgeMultiHopProof`
//! snarks that walk the referenced-block chain back to a thread-0 block that
//! the primary attestation lane can anchor. Same-thread claims produce an
//! `EventBundle` with `hop_blobs.is_empty()`.
//!
//! Not to be confused with
//! `bridge_event_prove_circuit::bundle_verifier::BundleProof` — that is a
//! per-snark PI container used by the spec-executable off-circuit
//! verifier `verify_bundle`. `EventBundle` here is the daemon-facing output:
//! the whole bundle's raw proof blobs plus their public instances.
//!
//! Blob bytes are raw SHPLONK proof bytes (as produced by
//! `snark-verifier-sdk`), not hex — hex-encoding is a JSON-boundary
//! concern owned by the daemon's `proof_event_<N>.json` writer.

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

/// A whole event-proof bundle as produced by the prover.
///
/// `hop_blobs` is `[]` for same-thread events. Non-empty length must satisfy
/// `hop_blobs.len() <= bridge_event_witness::schema::N_BUNDLE_MAX`; enforcement
/// lives in the prover / on-chain verifier, not this container.
pub struct EventBundle {
    /// Circuit 4 (`BridgeEventFinalProof`) proof bytes.
    pub final_blob: Vec<u8>,
    /// Public instances the final blob commits to
    /// (`FINAL_PI_LEN` = 13 scalars post-migration).
    pub final_public_instances: Vec<Fr>,
    /// Ordered `BridgeMultiHopProof` snarks. Element `i` starts at
    /// `hop_public_instances[i][0]` (`hopStartBlockId`) and ends at
    /// `hop_public_instances[i][1]` (`hopEndBlockId`). The on-chain adapter
    /// glues adjacent snarks by clear block-id equality; there is no salt.
    pub hop_blobs: Vec<Vec<u8>>,
    /// Per-snark public instances, indexed 1:1 with `hop_blobs`. Each entry
    /// has `MULTI_HOP_PI_LEN` = 2 scalars: `[hopStartBlockId, hopEndBlockId]`.
    pub hop_public_instances: Vec<Vec<Fr>>,
}

impl EventBundle {
    /// True when this bundle carries no hop snarks — i.e. the event is
    /// same-thread and the final proof anchors directly.
    pub fn is_same_thread(&self) -> bool {
        self.hop_blobs.is_empty()
    }
}
