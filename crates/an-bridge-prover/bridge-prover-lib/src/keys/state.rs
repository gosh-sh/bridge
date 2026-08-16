//! Shared per-circuit key-manager state. Composed by each per-circuit
//! manager (`PrimaryKeyManager`, `FallbackKeyManager`,
//! `LayerHashesKeyManager`, `EventKeyManager`) so that the mechanical
//! parts of the lifecycle — SRS load, cache-hit check, keygen
//! timing/logging, save-and-set, on-demand PK load/unload, accessor
//! panic messages — live in exactly one place. Per-circuit files only
//! carry their circuit-specific bits (witness construction inside
//! `ensure_keys`, degree constants, and any extra accessors).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, Circuit, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::info;

use super::common::{
    load_config, load_srs, pk_path, save_config, save_pk, save_vk, try_load_pk, try_load_vk,
};

/// SRS + optional VK/PK/config cache with disk-backed lifecycle.
pub(crate) struct KeyManagerState {
    params_dir: PathBuf,
    prefix: &'static str,
    /// Operator-facing hint appended to the `load_pk` log line (e.g.
    /// `"~3.7 GB"`). Purely cosmetic — wall-clock elapsed is always logged
    /// after the load regardless.
    pk_size_hint: Option<&'static str>,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
}

impl KeyManagerState {
    /// Construct: load SRS at `srs_k` (may exceed circuit `k` when the
    /// ceremony was oversized — see `LayerHashesKeyManager::KEYGEN_SRS_K`
    /// / `EventKeyManager::KEYGEN_SRS_K`), best-effort load any cached
    /// config/VK from disk; log if PK file is present (loaded on demand).
    pub(crate) fn new(
        params_dir: &Path,
        prefix: &'static str,
        k: u32,
        srs_k: u32,
        pk_size_hint: Option<&'static str>,
    ) -> Self {
        std::fs::create_dir_all(params_dir).ok();
        let srs = load_srs(params_dir, srs_k);
        let mut state = Self {
            params_dir: params_dir.to_path_buf(),
            prefix,
            pk_size_hint,
            srs,
            k,
            vk: None,
            pk: None,
            config: None,
        };
        if let Ok(config) = load_config(&state.params_dir, prefix) {
            info!("found {} config: {:?}", prefix, config);
            if let Some(vk) = try_load_vk(&state.params_dir, prefix, &config) {
                info!("loaded {} VK from cache", prefix);
                state.vk = Some(vk);
            }
            if pk_path(&state.params_dir, prefix).exists() {
                info!("{} PK found on disk (will load on demand)", prefix);
            }
            state.config = Some(config);
        }
        state
    }

    /// True iff `ensure_keys` can skip regeneration: VK in memory AND PK
    /// present on disk.
    pub(crate) fn keys_cached(&self) -> bool {
        self.vk.is_some() && pk_path(&self.params_dir, self.prefix).exists()
    }

    /// Run keygen_vk + keygen_pk with prefix-labelled timing/logging, then
    /// persist VK + PK + config to disk and install VK + config into `self`.
    /// The PK is intentionally dropped after save (freeing ~2.8–3.7 GB for
    /// the BLS circuits); callers reload on demand via [`Self::load_pk`].
    pub(crate) fn run_keygen<C>(
        &mut self,
        circuit: &C,
        base_params: BaseCircuitParams,
    ) -> anyhow::Result<()>
    where
        C: Circuit<Fr>,
    {
        info!("{} base_circuit_params: {:?}", self.prefix, base_params);

        let t = Instant::now();
        let vk = keygen_vk(&self.srs, circuit)
            .with_context(|| format!("{} keygen_vk failed", self.prefix))?;
        info!("{} keygen_vk: {:?}", self.prefix, t.elapsed());

        let t = Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), circuit)
            .with_context(|| format!("{} keygen_pk failed", self.prefix))?;
        info!("{} keygen_pk: {:?}", self.prefix, t.elapsed());

        save_vk(&self.params_dir, self.prefix, &vk)?;
        save_pk(&self.params_dir, self.prefix, &pk)?;
        save_config(&self.params_dir, self.prefix, &base_params)?;

        self.vk = Some(vk);
        self.config = Some(base_params);
        // `pk` drops here — memory freed; reload on demand via `load_pk`.
        Ok(())
    }

    /// Load PK from disk into memory. Idempotent — no-op if already loaded.
    /// Requires `ensure_keys` (or a cached config on disk) to have populated
    /// `self.config` first.
    pub(crate) fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("{} config not loaded — run ensure_keys first", self.prefix)
        })?;
        match self.pk_size_hint {
            Some(hint) => {
                info!("loading {} PK from disk ({})...", self.prefix, hint)
            }
            None => info!("loading {} PK from disk...", self.prefix),
        }
        let t = Instant::now();
        let pk = try_load_pk(&self.params_dir, self.prefix, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load {} PK from {}",
                self.prefix,
                pk_path(&self.params_dir, self.prefix).display()
            )
        })?;
        info!("{} PK loaded in {:?}", self.prefix, t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    /// Drop PK from memory. Cheap idempotent no-op if already unloaded.
    pub(crate) fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("{} PK unloaded from memory", self.prefix);
        }
    }

    // ---- accessors ----

    pub(crate) fn params_dir(&self) -> &Path {
        &self.params_dir
    }
    pub(crate) fn srs(&self) -> &ParamsKZG<Bn256> {
        &self.srs
    }
    pub(crate) fn k(&self) -> u32 {
        self.k
    }
    pub(crate) fn vk_opt(&self) -> Option<&VerifyingKey<G1Affine>> {
        self.vk.as_ref()
    }
    pub(crate) fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.vk
            .as_ref()
            .unwrap_or_else(|| panic!("{} VK not loaded", self.prefix))
    }
    pub(crate) fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk
            .as_ref()
            .unwrap_or_else(|| panic!("{} PK not loaded", self.prefix))
    }
    pub(crate) fn config(&self) -> &BaseCircuitParams {
        self.config
            .as_ref()
            .unwrap_or_else(|| panic!("{} config not loaded", self.prefix))
    }
}
