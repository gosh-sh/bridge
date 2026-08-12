//! Circuit 1A (Primary Attestation) key manager.
//!
//! Owns the SRS + cached VK/PK/config for the primary BLS-attestation
//! circuit. PK stays on disk between calls to
//! [`PrimaryKeyManager::load_pk`] and [`PrimaryKeyManager::unload_pk`] to
//! avoid holding ~3.7 GB in memory when it isn't needed.
//!
//! Lifecycle (SRS load, cache-hit check, keygen timing/logging, save-and-set,
//! on-demand PK load/unload, accessor plumbing) is delegated to the shared
//! [`super::common::KeyManagerState`]. Only the circuit-specific
//! reference-witness construction lives here.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use attestation_bls_checker_circuit::primary_circuit::PrimaryAttestationBlsCheckerCircuit;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::common::KeyManagerState;

pub(super) const PREFIX: &str = "primary";

/// Operator-facing size hint for `load_pk`. Purely cosmetic — the actual
/// elapsed time is always logged after the load.
const PK_SIZE_HINT: &str = "~3.7 GB";

// Circuit shape constants shared with the fallback circuit (both circuits
// use the same K/lookup/limb sizes, only the constraint system differs).
pub(super) const NUM_UNUSABLE_ROWS: usize = 109;
pub(super) const LOOKUP_BITS: usize = 19;
pub(super) const LIMB_BITS: usize = 104;
pub(super) const NUM_LIMBS: usize = 5;
pub(super) const MAX_SIGNERS: usize = 300;

pub struct PrimaryKeyManager {
    state: KeyManagerState,
}

impl PrimaryKeyManager {
    /// SHPLONK-production default degree.
    pub const DEFAULT_K: u32 = 20;

    /// Convenience constructor at [`Self::DEFAULT_K`].
    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

    /// Full constructor. Loads SRS (halo2-base disk cache) and any cached
    /// VK/config from `params_dir`. PK is left on disk — call
    /// [`Self::load_pk`] before proving.
    pub fn new_with_k(params_dir: &Path, k: u32) -> Self {
        Self {
            state: KeyManagerState::new(params_dir, PREFIX, k, k, Some(PK_SIZE_HINT)),
        }
    }

    /// Ensure circuit keys exist on disk. Runs keygen if not cached.
    /// PK is NOT kept in memory — call [`Self::load_pk`] before proving.
    pub fn ensure_keys(&mut self, bk_set: &HashMap<u16, Vec<u8>>) -> anyhow::Result<()> {
        if self.state.keys_cached() {
            info!("primary keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for primary circuit (this may take ~60s)...");

        let test_data =
            bridge_test_data_gen::generator::generate_test_data_all_sign(bk_set.len())
                .context("failed to generate reference test data for keygen")?;

        let last_seen: u32 = 0;
        let circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            test_data.bk_set,
            last_seen,
            self.state.k() as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );
        let base_params = circuit.params.base_circuit_params.clone();

        self.state.run_keygen(&circuit, base_params)?;
        info!("primary keys generated and cached (PK on disk, not in memory)");
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

/// Public shape constants — historically exposed by
/// `bridge_prover_lib::keys::circuit_*()` helpers.
pub const fn circuit_k() -> u32 {
    PrimaryKeyManager::DEFAULT_K
}
pub const fn circuit_limb_bits() -> usize {
    LIMB_BITS
}
pub const fn circuit_num_limbs() -> usize {
    NUM_LIMBS
}
pub const fn circuit_max_signers() -> usize {
    MAX_SIGNERS
}
pub const fn circuit_num_unusable_rows() -> usize {
    NUM_UNUSABLE_ROWS
}
pub const fn circuit_lookup_bits() -> usize {
    LOOKUP_BITS
}
