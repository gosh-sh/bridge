//! Fallback (Circuit 1B) key management.
//!
//! Mirrors `bridge_prover_lib::keys::KeyManager` but for `FallbackAttestationBlsCheckerCircuit`.
//! Uses the same K, lookup bits, limb sizes, and SerdeFormat as the partner's primary path so
//! a single `kzg_bn254_20.srs` file is shared.
//!
//! On disk (under `params_dir`):
//! - `kzg_bn254_{K}.srs` — **Hermez Perpetual Powers of Tau (BN254, K=20 slice)** SRS,
//!   loaded by `halo2_base::utils::fs::gen_srs` (shared with primary).
//!   Provenance: `powersOfTau28_hez_final.ptau` → `han0110/halo2-kzg-srs`
//!   `convert-from-snarkjs` → raw halo2 canonical format, validated via
//!   `same_ratio` (`e(g[1], g2) == e(g[0], s_g2)`). SHA-256:
//!   `80394564e2598883dbb5d7d61630287f34e29cdd806d7ef74f68acc6bffeb608`.
//!   If the cached file is missing, `gen_srs` falls back to a **test**
//!   deterministic SRS (known trapdoor) — make sure the ceremony file is
//!   present before generating production keys.
//! - `fallback_vk.bin`
//! - `fallback_pk.bin`
//! - `fallback_config_params.json`

use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::Context;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
    SerdeFormat,
};
use halo2_base::utils::fs::gen_srs;
use tracing::info;

use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;

use crate::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers,
    circuit_num_limbs, circuit_num_unusable_rows,
};

const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;
const PREFIX: &str = "fallback";

/// Holds SRS, VK, PK, and the cached `BaseCircuitParams` for Circuit 1B.
pub struct FallbackKeyManager {
    pub params_dir: PathBuf,
    pub srs: ParamsKZG<Bn256>,
    pub vk: Option<VerifyingKey<G1Affine>>,
    pub pk: Option<ProvingKey<G1Affine>>,
    pub config: Option<BaseCircuitParams>,
}

impl FallbackKeyManager {
    /// Create a new key manager. Loads SRS (cached on disk by halo2-base), then attempts
    /// to load `fallback_*.bin` artifacts from disk if they exist. Does NOT run keygen
    /// proactively — call [`ensure_keys`] before generating proofs.
    pub fn new(params_dir: &Path) -> Self {
        std::fs::create_dir_all(params_dir).ok();

        // gen_srs reads PARAMS_DIR for caching; mirror partner's pattern exactly.
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_var("PARAMS_DIR", params_dir.to_str().unwrap());
        let srs = gen_srs(circuit_k());
        std::env::set_current_dir(&prev_dir).ok();

        let mut mgr = Self {
            params_dir: params_dir.to_path_buf(),
            srs,
            vk: None,
            pk: None,
            config: None,
        };

        if let Ok(config) = mgr.load_config() {
            info!(?config, "found cached fallback config");
            if let Some(vk) = mgr.try_load_vk(&config) {
                info!("loaded fallback VK from cache");
                mgr.vk = Some(vk);
                if let Some(pk) = mgr.try_load_pk(&config) {
                    info!("loaded fallback PK from cache");
                    mgr.pk = Some(pk);
                }
            }
            mgr.config = Some(config);
        }

        mgr
    }

    /// Ensure VK + PK exist for Circuit 1B. If not cached, runs keygen against a synthetic
    /// reference circuit sized to `bk_set.len()` (mirrors partner's `ensure_primary_keys`).
    /// Keygen takes ~2-5 minutes the first time and produces a multi-GB PK on disk.
    pub fn ensure_keys(&mut self, bk_set: &HashMap<u16, Vec<u8>>) -> anyhow::Result<()> {
        if self.vk.is_some() && self.pk.is_some() {
            info!("fallback keys already loaded");
            return Ok(());
        }

        info!("running fallback keygen (this may take a few minutes)…");

        // Build a reference circuit using synthetic test data of the same shape as `bk_set`.
        let test_data =
            bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(bk_set.len())
                .context("failed to generate reference fallback test data for keygen")?;
        let attestation_2_bytes = test_data
            .attestation_2_bytes
            .context("fallback test data must have attestation_2_bytes")?;

        let last_seen: u32 = 0;
        let circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            attestation_2_bytes,
            test_data.bk_set,
            last_seen,
            circuit_k() as usize,
            circuit_num_unusable_rows(),
            circuit_lookup_bits(),
            circuit_limb_bits(),
            circuit_num_limbs(),
            circuit_max_signers(),
        );
        let base_params = circuit.params.base_circuit_params.clone();
        info!(?base_params, "fallback base_circuit_params");

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("fallback keygen_vk failed")?;
        info!(elapsed = ?t.elapsed(), "fallback keygen_vk done");

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit).context("fallback keygen_pk failed")?;
        info!(elapsed = ?t.elapsed(), "fallback keygen_pk done");

        self.save_vk(&vk)?;
        self.save_pk(&pk)?;
        self.save_config(&base_params)?;

        self.vk = Some(vk);
        self.pk = Some(pk);
        self.config = Some(base_params);

        info!("fallback keys generated and cached");
        Ok(())
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

    // ---- internal disk I/O ----

    fn vk_path(&self) -> PathBuf {
        self.params_dir.join(format!("{PREFIX}_vk.bin"))
    }
    fn pk_path(&self) -> PathBuf {
        self.params_dir.join(format!("{PREFIX}_pk.bin"))
    }
    fn config_path(&self) -> PathBuf {
        self.params_dir.join(format!("{PREFIX}_config_params.json"))
    }

    fn load_config(&self) -> anyhow::Result<BaseCircuitParams> {
        let data = std::fs::read_to_string(self.config_path())?;
        Ok(serde_json::from_str(&data)?)
    }

    fn save_config(&self, config: &BaseCircuitParams) -> anyhow::Result<()> {
        std::fs::write(self.config_path(), serde_json::to_string_pretty(config)?)?;
        Ok(())
    }

    fn try_load_vk(&self, config: &BaseCircuitParams) -> Option<VerifyingKey<G1Affine>> {
        let file = std::fs::File::open(self.vk_path()).ok()?;
        let mut reader = BufReader::new(file);
        VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }

    fn try_load_pk(&self, config: &BaseCircuitParams) -> Option<ProvingKey<G1Affine>> {
        let file = std::fs::File::open(self.pk_path()).ok()?;
        let mut reader = BufReader::new(file);
        ProvingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }

    fn save_vk(&self, vk: &VerifyingKey<G1Affine>) -> anyhow::Result<()> {
        let file = std::fs::File::create(self.vk_path())?;
        let mut writer = BufWriter::new(file);
        vk.write(&mut writer, SERDE_FMT)?;
        Ok(())
    }

    fn save_pk(&self, pk: &ProvingKey<G1Affine>) -> anyhow::Result<()> {
        let file = std::fs::File::create(self.pk_path())?;
        let mut writer = BufWriter::new(file);
        pk.write(&mut writer, SERDE_FMT)?;
        Ok(())
    }
}
