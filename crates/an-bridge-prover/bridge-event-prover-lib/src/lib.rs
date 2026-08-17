//! Circuit 4 (Event Prove — `WithdrawalInitiated`) prover library
//! (`bridge-event-prover-lib`).
//!
//! Originally lived as `bridge_prover_lib::event_prover` + `event_verifier`;
//! split out so the prover/verifier surface for Circuit 4 is its own crate,
//! while the PK/VK/SRS lifecycle stays in
//! `bridge_prover_lib::keys::EventKeyManager` (a per-circuit sub-manager of
//! the monolithic `KeyManager` facade).
//!
//! Two public surfaces:
//!   * Free functions [`prover::build_proof_inputs`], [`prover::generate_event_proof`],
//!     [`prover::generate_event_proof_from_circuit`], [`verifier::verify_event_proof`]
//!     for direct use. They take `&EventKeyManager` — callers who hold the
//!     multi-circuit facade pass `&km.event`.
//!   * [`EventProver`] — a thin wrapper that borrows `&mut EventKeyManager`
//!     and groups the methods together.

pub mod prover;
pub mod verifier;

pub use prover::{
    build_proof_inputs, default_event_circuit_params, generate_event_proof,
    generate_event_proof_from_circuit, EventProofInputs, EventProofOutput,
    // Witness-schema re-exports so consumers don't need a direct dep.
    AnchorRef, BlockContext, CellRecord, DenseChainLinkSer, MerkleProofData,
    PrivateWitness, WithdrawalInitiated, SCHEMA_VERSION,
};
pub use verifier::verify_event_proof;

use anyhow::Result;
use bridge_event_prove_circuit::bridge_event_prove_circuit::BridgeEventProveCircuit;
use bridge_prover_lib::keys::EventKeyManager;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

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
        Self { ekm }
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
    pub fn prove(&self, witness: &PrivateWitness) -> Result<EventProofOutput> {
        generate_event_proof(self.ekm, witness)
    }

    /// Generate a Circuit 4 proof from a pre-built circuit + instances.
    /// Caller must have called [`Self::load_pk`] first.
    pub fn prove_circuit(
        &self,
        circuit: BridgeEventProveCircuit,
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
