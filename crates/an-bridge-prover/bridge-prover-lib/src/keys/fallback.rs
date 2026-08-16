//! Circuit 1B (Fallback Attestation) key manager.
//!
//! Same lookup/limb shape as [`super::primary::PrimaryKeyManager`], but a
//! different constraint system (two BLS envelopes + `ThresholdMode::Fallback`
//! >N/2 threshold), so a distinct VK/PK.
//!
//! ## Why the default degree is `K = 21`, not `K = 20`
//!
//! At `K = 20` halo2-lib auto-configures Circuit 1B to ~44 advice columns
//! (roughly double primary's 24), which pushes the R15 SHPLONK aggregator's
//! Yul verifier over the 24,576-byte EIP-170 limit (~28.4 KB). Bumping the
//! inner circuit to `K = 21` halves the columns to ~22 and drops the
//! aggregator Yul to ~21.5 KB. Cf. `docs/r15_verifier_sizing_report.md`
//! and the `contracts/ethereum/verifiers/README.md` sizing table.
//!
//! The degree-21 KZG SRS shares tau with the degree-20 slice used by the
//! primary/layer paths, so on-chain aggregation stays consistent.
//!
//! Callers who want the legacy native-only `K = 20` fallback (pre-SHPLONK)
//! can construct via [`FallbackKeyManager::new_with_k`].
//!
//! Lifecycle (SRS load, cache-hit check, keygen timing/logging, save-and-set,
//! on-demand PK load/unload, accessor plumbing) is delegated to the shared
//! [`super::state::KeyManagerState`]. Only the circuit-specific
//! reference-witness construction lives here.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::state::KeyManagerState;
use super::primary::{LIMB_BITS, LOOKUP_BITS, MAX_SIGNERS, NUM_LIMBS, NUM_UNUSABLE_ROWS};

pub(super) const PREFIX: &str = "fallback";

/// Operator-facing size hint for `load_pk`. Purely cosmetic — the actual
/// elapsed time is always logged after the load.
const PK_SIZE_HINT: &str = "~3.7 GB";

pub struct FallbackKeyManager {
    state: KeyManagerState,
}

impl FallbackKeyManager {
    /// SHPLONK-production default degree.
    pub const DEFAULT_K: u32 = 21;

    /// Convenience constructor at [`Self::DEFAULT_K`].
    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

    /// Full constructor. Loads SRS (halo2-base disk cache) and any cached
    /// VK/config. PK is left on disk — call [`Self::load_pk`] before proving.
    pub fn new_with_k(params_dir: &Path, k: u32) -> Self {
        Self {
            state: KeyManagerState::new(params_dir, PREFIX, k, k, Some(PK_SIZE_HINT)),
        }
    }

    /// Ensure keys exist on disk. Runs keygen (a few minutes) if not cached.
    /// The keygen witness is synthetic; only the constraint shape affects
    /// the VK/PK, so any admissible input drives keygen identically.
    pub fn ensure_keys(&mut self, bk_set: &HashMap<u16, Vec<u8>>) -> anyhow::Result<()> {
        if self.state.keys_cached() {
            info!("fallback keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for fallback circuit (this may take a few minutes)...");

        let test_data =
            bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(bk_set.len())
                .context("failed to generate reference fallback test data for keygen")?;
        let attestation_fallback_bytes = test_data.attestation_2_bytes.ok_or_else(|| {
            anyhow::format_err!(
                "fallback test data missing attestation_2_bytes (FALLBACK target proof)"
            )
        })?;

        let last_seen: u32 = 0;
        let circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            attestation_fallback_bytes,
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
        info!("fallback keys generated and cached (PK on disk, not in memory)");
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
