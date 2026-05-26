//! Phase 1.B — Circuit 2 (Layer Hashes Movement) key management.
//!
//! Mirrors [`crate::keys::FallbackKeyManager`] but for
//! `LayerHashesMovementCheckerCircuit` from
//! `historical-layer-hashes-movement-checker-circuit`. Unlike Circuit 1A/1B,
//! Circuit 2 uses **K = 17, LOOKUP_BITS = 16, NUM_UNUSABLE_ROWS = 109** and is
//! ~8× faster + ~8× smaller in proof bytes; it has its own SRS
//! (`kzg_bn254_17.srs`) that `halo2-base::utils::fs::gen_srs` will lazily
//! populate alongside the K=20 file used by 1A/1B.
//!
//! On disk under `params_dir`:
//! - `kzg_bn254_17.srs`
//! - `layer_hashes_vk.bin`
//! - `layer_hashes_pk.bin`
//! - `layer_hashes_config_params.json`

use std::{
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use anyhow::Context;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::{
    gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
        poly::kzg::commitment::ParamsKZG,
        SerdeFormat,
    },
    utils::fs::gen_srs,
};
use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit,
    test_helpers::{
        K as LH_K, LOOKUP_BITS as LH_LOOKUP_BITS, NUM_UNUSABLE_ROWS as LH_NUM_UNUSABLE_ROWS,
    },
    LAYER_PREIMAGE_SIZE, NUM_MERKLE_SIBLINGS,
};
use tracing::info;

const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;
const PREFIX: &str = "layer_hashes";

/// Reference fixture used to derive `BaseCircuitParams` during keygen.
///
/// The partner's circuit's `BaseCircuitParams` are derived from the actual
/// witness shape, but for layer-hashes that shape only depends on
/// `MAX_LAYERS = 10`, `NUM_MERKLE_SIBLINGS = 3`, and `MAX_CHAIN_LEN = 11`
/// (all compile-time constants), so the `BaseCircuitParams` is stable across
/// every `(num_layers, num_chain_steps)` combination once `MAX_*` are fixed.
/// We just need *some* valid witness to drive `calculate_params`.
pub struct LayerHashesReferenceWitness {
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: Vec<DenseChainLink>,
    pub bk_set_poseidon_hash: Fr,
}

/// Holds SRS, VK, PK, and the cached `BaseCircuitParams` for Circuit 2.
pub struct LayerHashesKeyManager {
    pub params_dir: PathBuf,
    pub srs: ParamsKZG<Bn256>,
    pub vk: Option<VerifyingKey<G1Affine>>,
    pub pk: Option<ProvingKey<G1Affine>>,
    pub config: Option<BaseCircuitParams>,
}

impl LayerHashesKeyManager {
    pub fn new(params_dir: &Path) -> Self {
        std::fs::create_dir_all(params_dir).ok();

        // gen_srs reads PARAMS_DIR for caching; mirror Phase 1.A pattern.
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_var("PARAMS_DIR", params_dir.to_str().unwrap());
        let srs = gen_srs(LH_K);
        std::env::set_current_dir(&prev_dir).ok();

        let mut mgr = Self {
            params_dir: params_dir.to_path_buf(),
            srs,
            vk: None,
            pk: None,
            config: None,
        };

        if let Ok(config) = mgr.load_config() {
            info!(?config, "found cached layer-hashes config");
            if let Some(vk) = mgr.try_load_vk(&config) {
                info!("loaded layer-hashes VK from cache");
                mgr.vk = Some(vk);
                if let Some(pk) = mgr.try_load_pk(&config) {
                    info!("loaded layer-hashes PK from cache");
                    mgr.pk = Some(pk);
                }
            }
            mgr.config = Some(config);
        }

        mgr
    }

    /// Ensure VK + PK exist for Circuit 2. If not cached, runs keygen against
    /// `reference`. Keygen for Circuit 2 is ~5× faster than Circuit 1A (K=17
    /// vs K=20, smaller advice/lookup footprint).
    pub fn ensure_keys(&mut self, reference: &LayerHashesReferenceWitness) -> anyhow::Result<()> {
        if self.vk.is_some() && self.pk.is_some() {
            info!("layer-hashes keys already loaded");
            return Ok(());
        }

        info!("running layer-hashes keygen…");

        let circuit = LayerHashesMovementCheckerCircuit::new(
            reference.layer_hashes_preimage,
            reference.merkle_siblings,
            reference.prev_max_level_layer_hash,
            reference.num_prev_chain_steps,
            reference.prev_chain_proofs.clone(),
            reference.bk_set_poseidon_hash,
            LH_K as usize,
            LH_NUM_UNUSABLE_ROWS,
            LH_LOOKUP_BITS,
        );
        let base_params = circuit.base_circuit_params().clone();
        info!(?base_params, "layer-hashes base_circuit_params");

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("layer-hashes keygen_vk failed")?;
        info!(elapsed = ?t.elapsed(), "layer-hashes keygen_vk done");

        let t = std::time::Instant::now();
        let pk =
            keygen_pk(&self.srs, vk.clone(), &circuit).context("layer-hashes keygen_pk failed")?;
        info!(elapsed = ?t.elapsed(), "layer-hashes keygen_pk done");

        self.save_vk(&vk)?;
        self.save_pk(&pk)?;
        self.save_config(&base_params)?;

        self.vk = Some(vk);
        self.pk = Some(pk);
        self.config = Some(base_params);

        info!("layer-hashes keys generated and cached");
        Ok(())
    }

    pub fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.vk.as_ref().expect("layer-hashes VK not loaded")
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk.as_ref().expect("layer-hashes PK not loaded")
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.config
            .as_ref()
            .expect("layer-hashes config not loaded")
    }

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

/// Convenience constants — re-exported so callers don't need to depend on
/// `historical_layer_hashes_movement_checker_circuit::test_helpers` directly.
pub const LAYER_HASHES_K: u32 = LH_K;
pub const LAYER_HASHES_LOOKUP_BITS: usize = LH_LOOKUP_BITS;
pub const LAYER_HASHES_NUM_UNUSABLE_ROWS: usize = LH_NUM_UNUSABLE_ROWS;
