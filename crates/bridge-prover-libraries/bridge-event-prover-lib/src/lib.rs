//! Circuit 4 (Event Prove — `WithdrawalInitiated`) prover library
//! (`bridge-event-prover-lib`).
//!
//! Two public surfaces:
//!   * Free functions [`prover::build_proof_inputs`],
//!     [`prover::generate_event_proof`],
//!     [`prover::generate_event_proof_from_circuit`],
//!     [`verifier::verify_event_proof`] for direct use. They take
//!     `&EventKeyManager` — callers who hold the multi-circuit facade pass
//!     `&km.event`.
//!   * [`EventProver`] — a thin wrapper that borrows `&mut EventKeyManager` and
//!     groups the methods together.

pub mod bundle;
pub mod prover;
pub mod verifier;

use anyhow::Result;
use bridge_event_prove_circuit::bridge_event_final_proof::BridgeEventFinalProof;
use bridge_prover_lib::keys::{EventKeyManager, MultiHopKeyManager};
pub use bundle::EventBundle;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
pub use prover::{
    build_multi_hop_witness_from_json,
    build_proof_inputs,
    default_event_circuit_params,
    generate_event_bundle,
    generate_event_proof,
    generate_event_proof_from_circuit,
    generate_multi_hop_proof,
    generate_multi_hop_proof_from_circuit,
    generate_multi_hop_proof_from_circuit_with_transcript,
    generate_multi_hop_proof_with_transcript,
    // Witness-schema re-exports so consumers don't need a direct dep.
    AnchorRef,
    BlockContext,
    CellRecord,
    DenseChainLinkSer,
    EventProofInputs,
    EventProofOutput,
    HopWitnessJson,
    MerkleProofData,
    MultiHopBundleWitnessJson,
    MultiHopProofOutput,
    MultiHopProofWitnessJson,
    PrivateWitness,
    WithdrawalInitiated,
    SCHEMA_VERSION,
};
pub use verifier::{verify_bundle, verify_event_proof, verify_multi_hop_proof, BundleVerifyError};

/// Thin wrapper around `&mut EventKeyManager` that groups the Circuit 4
/// surface (ensure_keys / load_pk / prove / verify / unload_pk).
///
/// All key/SRS storage is owned by the borrowed `EventKeyManager` — this
/// struct holds no state of its own.
pub struct EventProver<'a> {
    ekm: &'a mut EventKeyManager,
}

impl<'a> EventProver<'a> {
    pub fn new(ekm: &'a mut EventKeyManager) -> Self {
        Self {
            ekm,
        }
    }

    /// Borrow the underlying `EventKeyManager`.
    pub fn event_key_manager(&self) -> &EventKeyManager {
        self.ekm
    }

    pub fn event_key_manager_mut(&mut self) -> &mut EventKeyManager {
        self.ekm
    }

    /// Ensure event circuit keys exist on disk (runs keygen on first call).
    pub fn ensure_keys(&mut self) -> Result<()> {
        self.ekm.ensure_keys()
    }

    /// Load event PK from disk into memory. Must be called before `prove*`.
    pub fn load_pk(&mut self) -> Result<()> {
        self.ekm.load_pk()
    }

    /// Unload event PK from memory.
    pub fn unload_pk(&mut self) {
        self.ekm.unload_pk()
    }

    /// Generate a Circuit 4 proof from a fully-populated [`PrivateWitness`].
    /// Caller must have called [`Self::load_pk`] first.
    ///
    /// `hop_bundle` supplies the `y_block_id` endpoint — pass
    /// `&MultiHopBundleWitnessJson::default()` for same-thread claims.
    pub fn prove(
        &self,
        witness: &PrivateWitness,
        hop_bundle: &MultiHopBundleWitnessJson,
    ) -> Result<EventProofOutput> {
        generate_event_proof(self.ekm, witness, hop_bundle)
    }

    /// Generate a Circuit 4 proof from a pre-built circuit + instances.
    /// Caller must have called [`Self::load_pk`] first.
    pub fn prove_circuit(
        &self,
        circuit: BridgeEventFinalProof,
        public_instances: Vec<Fr>,
    ) -> Result<EventProofOutput> {
        generate_event_proof_from_circuit(self.ekm, circuit, public_instances)
    }

    /// Verify a Circuit 4 proof. The event VK is loaded into memory by
    /// `ensure_keys` and stays there — no separate load step.
    pub fn verify(&self, proof_bytes: &[u8], instances: &[Fr]) -> bool {
        verify_event_proof(self.ekm, proof_bytes, instances)
    }
}

/// Thin wrapper that borrows both the event `KeyManager` and the multi-hop
/// `KeyManager`, so callers can drive a whole cross-thread bundle
/// (final `BridgeEventFinalProof` + 0..=`N_BUNDLE_MAX`
/// `BridgeMultiHopProof` snarks) through one struct.
///
/// Same-thread callers who never touch hops should keep using
/// [`EventProver`] and skip the multi-hop keygen entirely.
pub struct BundleProver<'a> {
    ekm: &'a mut EventKeyManager,
    mhkm: &'a mut MultiHopKeyManager,
}

impl<'a> BundleProver<'a> {
    pub fn new(ekm: &'a mut EventKeyManager, mhkm: &'a mut MultiHopKeyManager) -> Self {
        Self {
            ekm,
            mhkm,
        }
    }

    pub fn event_key_manager(&self) -> &EventKeyManager {
        self.ekm
    }

    pub fn multi_hop_key_manager(&self) -> &MultiHopKeyManager {
        self.mhkm
    }

    /// Ensure both event and multi-hop circuit keys exist on disk.
    pub fn ensure_keys(&mut self) -> Result<()> {
        self.ekm.ensure_keys()?;
        self.mhkm.ensure_keys()
    }

    /// Load both PKs into memory. Must be called before `prove_bundle`.
    pub fn load_pks(&mut self) -> Result<()> {
        self.ekm.load_pk()?;
        self.mhkm.load_pk()
    }

    /// Drop both PKs from memory.
    pub fn unload_pks(&mut self) {
        self.ekm.unload_pk();
        self.mhkm.unload_pk();
    }

    /// Prove a whole cross-thread bundle. `hop_bundle.snarks` may be empty for
    /// a same-thread event — in that case the returned [`EventBundle`] has
    /// `hop_blobs.is_empty()` and the caller should submit only the final
    /// blob via `withdrawByProof`.
    pub fn prove_bundle(
        &self,
        witness: &PrivateWitness,
        hop_bundle: &MultiHopBundleWitnessJson,
    ) -> Result<EventBundle> {
        generate_event_bundle(self.ekm, self.mhkm, witness, hop_bundle)
    }

    /// Verify a whole bundle end-to-end (structural + per-snark SHPLONK).
    /// See [`verify_bundle`] for the `anchor_ok` contract.
    pub fn verify_bundle(
        &self,
        bundle: &EventBundle,
        anchor_ok: impl Fn(&[u8; 32], u8) -> bool,
    ) -> Result<(), BundleVerifyError> {
        verify_bundle(self.ekm, self.mhkm, bundle, anchor_ok)
    }
}
