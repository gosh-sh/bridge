//! Circuit 4 (Event Prove — `WithdrawalInitiated`) key manager.
//!
//! Uses the deterministic synthetic-witness path from
//! `bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs`;
//! the produced circuit's constraint system is independent of witness
//! values, so the VK/PK shape is stable across machines and CI runs.
//!
//! Lifecycle (SRS load, cache-hit check, keygen timing/logging, save-and-set,
//! on-demand PK load/unload, accessor plumbing) is delegated to the shared
//! [`super::state::KeyManagerState`]. Only the circuit-specific
//! reference-witness construction lives here.

use std::path::Path;

use bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::state::KeyManagerState;

pub(super) const PREFIX: &str = "event";

/// Deterministic seed for the synthetic-witness keygen path. Any seed
/// produces the same VK/PK shape.
pub(super) const EVENT_KEYGEN_SEED: u64 = 0xE5E5_E5E5_E5E5_E5E5;

pub struct EventKeyManager {
    state: KeyManagerState,
}

impl EventKeyManager {
    /// Circuit-shape degree — matches
    /// `bridge_event_prove_circuit::test_helpers::K` and the `k` field of
    /// `event_prover::default_event_circuit_params`.
    pub const DEFAULT_K: u32 = 19;

    /// SRS degree used at keygen / prove / verify. Same rationale as
    /// [`super::LayerHashesKeyManager::KEYGEN_SRS_K`]: halo2-axiom bakes
    /// `params.k()` into `vk.domain`, and partner event PKs were keygen'd
    /// against the shared K=20 ceremony.
    pub const KEYGEN_SRS_K: u32 = 20;

    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

    pub fn new_with_k(params_dir: &Path, k: u32) -> Self {
        let srs_k = Self::KEYGEN_SRS_K.max(k);
        Self {
            state: KeyManagerState::new(params_dir, PREFIX, k, srs_k, None),
        }
    }

    pub fn ensure_keys(&mut self) -> anyhow::Result<()> {
        if self.state.keys_cached() {
            info!("event keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for event circuit (this may take a while)...");

        let (circuit, _instances) = build_synthetic_event_keygen_inputs(EVENT_KEYGEN_SEED);
        let base_params = circuit.base_circuit_params.clone();

        self.state.run_keygen(&circuit, base_params)?;
        info!("event keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    // ---- forwarding accessors / lifecycle ----

    pub fn load_pk(&mut self) -> anyhow::Result<()> {
        self.state.load_pk()
    }
    pub fn unload_pk(&mut self) {
        self.state.unload_pk()
    }
    pub fn params_dir(&self) -> &Path {
        self.state.params_dir()
    }
    pub fn srs(&self) -> &ParamsKZG<Bn256> {
        self.state.srs()
    }
    pub fn k(&self) -> u32 {
        self.state.k()
    }
    pub fn vk_opt(&self) -> Option<&VerifyingKey<G1Affine>> {
        self.state.vk_opt()
    }
    pub fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.state.vk()
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.state.pk()
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.state.config()
    }
}
