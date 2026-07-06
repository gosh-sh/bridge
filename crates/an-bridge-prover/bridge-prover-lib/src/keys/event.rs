//! Circuit 4 (Event Prove — `WithdrawalInitiated`) key manager.
//!
//! Uses the deterministic synthetic-witness path from
//! `bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs`;
//! the produced circuit's constraint system is independent of witness
//! values, so the VK/PK shape is stable across machines and CI runs.

use std::path::{Path, PathBuf};

use anyhow::Context;
use bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::common;

pub(super) const PREFIX: &str = "event";

/// Deterministic seed for the synthetic-witness keygen path. Any seed
/// produces the same VK/PK shape.
pub(super) const EVENT_KEYGEN_SEED: u64 = 0xE5E5_E5E5_E5E5_E5E5;

pub struct EventKeyManager {
    params_dir: PathBuf,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
}

impl EventKeyManager {
    /// Default degree — matches
    /// `bridge_event_prove_circuit::test_helpers::K` and the `k` field of
    /// `event_prover::default_event_circuit_params`. The Circuit 1 SRS at
    /// K=20 covers this smaller K too.
    pub const DEFAULT_K: u32 = 19;

    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

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
            info!("found event config: {:?}", config);
            if let Some(vk) = common::try_load_vk(&mgr.params_dir, PREFIX, &config) {
                info!("loaded event VK from cache");
                mgr.vk = Some(vk);
            }
            if common::pk_path(&mgr.params_dir, PREFIX).exists() {
                info!("event PK found on disk (will load on demand)");
            }
            mgr.config = Some(config);
        }
        mgr
    }

    pub fn ensure_keys(&mut self) -> anyhow::Result<()> {
        if self.vk.is_some() && common::pk_path(&self.params_dir, PREFIX).exists() {
            info!("event keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for event circuit (this may take a while)...");

        let (circuit, _instances) = build_synthetic_event_keygen_inputs(EVENT_KEYGEN_SEED);
        let base_params = circuit.base_circuit_params.clone();
        info!("event base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("event keygen_vk failed")?;
        info!("event keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk =
            keygen_pk(&self.srs, vk.clone(), &circuit).context("event keygen_pk failed")?;
        info!("event keygen_pk: {:?}", t.elapsed());

        common::save_vk(&self.params_dir, PREFIX, &vk)?;
        common::save_pk(&self.params_dir, PREFIX, &pk)?;
        common::save_config(&self.params_dir, PREFIX, &base_params)?;

        self.vk = Some(vk);
        self.config = Some(base_params);
        // pk dropped — reload on demand.

        info!("event keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("event config not loaded — run ensure_keys first")
        })?;
        info!("loading event PK from disk...");
        let t = std::time::Instant::now();
        let pk = common::try_load_pk(&self.params_dir, PREFIX, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load event PK from {}",
                common::pk_path(&self.params_dir, PREFIX).display()
            )
        })?;
        info!("event PK loaded in {:?}", t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    pub fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("event PK unloaded from memory");
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
        self.vk.as_ref().expect("event VK not loaded")
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk.as_ref().expect("event PK not loaded")
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.config.as_ref().expect("event config not loaded")
    }
}
