//! Circuit 1A (Primary Attestation) key manager.
//!
//! Owns the SRS + cached VK/PK/config for the primary BLS-attestation
//! circuit. PK stays on disk between calls to
//! [`PrimaryKeyManager::load_pk`] and [`PrimaryKeyManager::unload_pk`] to
//! avoid holding ~3.7 GB in memory when it isn't needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use attestation_bls_checker_circuit::primary_circuit::PrimaryAttestationBlsCheckerCircuit;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::common;

pub(super) const PREFIX: &str = "primary";

// Circuit shape constants shared with the fallback circuit (both circuits
// use the same K/lookup/limb sizes, only the constraint system differs).
pub(super) const NUM_UNUSABLE_ROWS: usize = 109;
pub(super) const LOOKUP_BITS: usize = 19;
pub(super) const LIMB_BITS: usize = 104;
pub(super) const NUM_LIMBS: usize = 5;
pub(super) const MAX_SIGNERS: usize = 300;

pub struct PrimaryKeyManager {
    params_dir: PathBuf,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
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
        std::fs::create_dir_all(params_dir).ok();
        let srs = common::load_srs(params_dir, k);
        let mut mgr = Self {
            params_dir: params_dir.to_path_buf(),
            srs,
            k,
            vk: None,
            pk: None,
            config: None,
        };
        if let Ok(config) = common::load_config(&mgr.params_dir, PREFIX) {
            info!("found primary config: {:?}", config);
            if let Some(vk) = common::try_load_vk(&mgr.params_dir, PREFIX, &config) {
                info!("loaded primary VK from cache");
                mgr.vk = Some(vk);
            }
            if common::pk_path(&mgr.params_dir, PREFIX).exists() {
                info!("primary PK found on disk (will load on demand)");
            }
            mgr.config = Some(config);
        }
        mgr
    }

    /// Ensure circuit keys exist on disk. Runs keygen if not cached.
    /// PK is NOT kept in memory — call [`Self::load_pk`] before proving.
    pub fn ensure_keys(&mut self, bk_set: &HashMap<u16, Vec<u8>>) -> anyhow::Result<()> {
        if self.vk.is_some() && common::pk_path(&self.params_dir, PREFIX).exists() {
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
            self.k as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );
        let base_params = circuit.params.base_circuit_params.clone();
        info!("primary base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("primary keygen_vk failed")?;
        info!("primary keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit)
            .context("primary keygen_pk failed")?;
        info!("primary keygen_pk: {:?}", t.elapsed());

        common::save_vk(&self.params_dir, PREFIX, &vk)?;
        common::save_pk(&self.params_dir, PREFIX, &pk)?;
        common::save_config(&self.params_dir, PREFIX, &base_params)?;

        self.vk = Some(vk);
        // pk dropped here — freed ~3.7 GB; load on demand via load_pk().
        self.config = Some(base_params);

        info!("primary keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    /// Load PK from disk into memory. Call before generating proofs.
    pub fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("primary config not loaded — run ensure_keys first")
        })?;
        info!("loading primary PK from disk (~3.7 GB)...");
        let t = std::time::Instant::now();
        let pk = common::try_load_pk(&self.params_dir, PREFIX, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load primary PK from {}",
                common::pk_path(&self.params_dir, PREFIX).display()
            )
        })?;
        info!("primary PK loaded in {:?}", t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    /// Drop PK from memory (frees ~3.7 GB). Cheap idempotent no-op if unloaded.
    pub fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("primary PK unloaded from memory");
        }
    }

    // ---- accessors ----

    pub fn params_dir(&self) -> &Path {
        &self.params_dir
    }
    pub fn srs(&self) -> &ParamsKZG<Bn256> {
        &self.srs
    }
    pub fn k(&self) -> u32 {
        self.k
    }
    pub fn vk_opt(&self) -> Option<&VerifyingKey<G1Affine>> {
        self.vk.as_ref()
    }
    pub fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.vk.as_ref().expect("primary VK not loaded")
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk.as_ref().expect("primary PK not loaded")
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.config.as_ref().expect("primary config not loaded")
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
