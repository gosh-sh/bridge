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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::common;
use super::primary::{LIMB_BITS, LOOKUP_BITS, MAX_SIGNERS, NUM_LIMBS, NUM_UNUSABLE_ROWS};

pub(super) const PREFIX: &str = "fallback";

pub struct FallbackKeyManager {
    params_dir: PathBuf,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
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
            info!("found fallback config: {:?}", config);
            if let Some(vk) = common::try_load_vk(&mgr.params_dir, PREFIX, &config) {
                info!("loaded fallback VK from cache");
                mgr.vk = Some(vk);
            }
            if common::pk_path(&mgr.params_dir, PREFIX).exists() {
                info!("fallback PK found on disk (will load on demand)");
            }
            mgr.config = Some(config);
        }
        mgr
    }

    /// Ensure keys exist on disk. Runs keygen (a few minutes) if not cached.
    /// The keygen witness is synthetic; only the constraint shape affects
    /// the VK/PK, so any admissible input drives keygen identically.
    pub fn ensure_keys(&mut self, bk_set: &HashMap<u16, Vec<u8>>) -> anyhow::Result<()> {
        if self.vk.is_some() && common::pk_path(&self.params_dir, PREFIX).exists() {
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
            self.k as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );
        let base_params = circuit.params.base_circuit_params.clone();
        info!("fallback base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("fallback keygen_vk failed")?;
        info!("fallback keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit)
            .context("fallback keygen_pk failed")?;
        info!("fallback keygen_pk: {:?}", t.elapsed());

        common::save_vk(&self.params_dir, PREFIX, &vk)?;
        common::save_pk(&self.params_dir, PREFIX, &pk)?;
        common::save_config(&self.params_dir, PREFIX, &base_params)?;

        self.vk = Some(vk);
        // pk dropped here — reload on demand via load_pk().
        self.config = Some(base_params);

        info!("fallback keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    /// Load PK from disk into memory. Call before proving.
    pub fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("fallback config not loaded — run ensure_keys first")
        })?;
        info!("loading fallback PK from disk (~3.7 GB)...");
        let t = std::time::Instant::now();
        let pk = common::try_load_pk(&self.params_dir, PREFIX, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load fallback PK from {}",
                common::pk_path(&self.params_dir, PREFIX).display()
            )
        })?;
        info!("fallback PK loaded in {:?}", t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    pub fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("fallback PK unloaded from memory");
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
        self.vk.as_ref().expect("fallback VK not loaded")
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk.as_ref().expect("fallback PK not loaded")
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.config.as_ref().expect("fallback config not loaded")
    }
}
