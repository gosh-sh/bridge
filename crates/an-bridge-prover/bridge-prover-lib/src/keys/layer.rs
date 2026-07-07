//! Circuit 2 (Layer-Hashes Movement) key manager.
//!
//! Independent from Circuit 1's shape — smaller K, tighter lookup width,
//! and its own reference-witness path built from
//! `bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use bridge_test_data_gen::layer_hashes::LayerHashChainData;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit, LAYER_PREIMAGE_SIZE, NUM_MERKLE_SIBLINGS,
};
use tracing::info;

use super::common;

pub(super) const PREFIX: &str = "layer";

pub(super) const NUM_UNUSABLE_ROWS: usize = 109;
pub(super) const LOOKUP_BITS: usize = 16;

pub struct LayerHashesKeyManager {
    params_dir: PathBuf,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
}

impl LayerHashesKeyManager {
    pub const DEFAULT_K: u32 = 17;

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
            info!("found layer config: {:?}", config);
            if let Some(vk) = common::try_load_vk(&mgr.params_dir, PREFIX, &config) {
                info!("loaded layer VK from cache");
                mgr.vk = Some(vk);
            }
            if common::pk_path(&mgr.params_dir, PREFIX).exists() {
                info!("layer PK found on disk (will load on demand)");
            }
            mgr.config = Some(config);
        }
        mgr
    }

    /// Ensure Circuit 2 keys exist. Runs keygen with a synthetic chain
    /// witness — the circuit shape is fixed (MAX_LAYERS / MAX_CHAIN_LEN,
    /// masked in-circuit) so any admissible witness drives keygen the same.
    ///
    /// REF_TREE_DEPTH is derived from the shared
    /// [`crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE`] so it stays in
    /// sync with the on-chain tree cadence.
    pub fn ensure_keys(&mut self) -> anyhow::Result<()> {
        if self.vk.is_some() && common::pk_path(&self.params_dir, PREFIX).exists() {
            info!("layer keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for layer circuit (this may take ~30s)...");

        const REF_NUM_LAYERS: usize = 3;
        const REF_NUM_PREV_CHAIN_STEPS: usize = 2;
        const REF_TREE_DEPTH: usize = (crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE + 2)
            .next_power_of_two()
            .trailing_zeros() as usize;
        let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(
            REF_NUM_LAYERS,
            REF_NUM_PREV_CHAIN_STEPS,
            REF_TREE_DEPTH,
        );
        let preimage = build_reference_preimage(&chain_data);
        // One opaque SHA sibling per depth level (NUM_MERKLE_SIBLINGS = 4
        // for the canonical 16-leaf block-id tree). Any admissible witness
        // drives keygen the same, so distinct-value bytes are fine.
        let mut siblings = [[0u8; 32]; NUM_MERKLE_SIBLINGS];
        for (i, sib) in siblings.iter_mut().enumerate() {
            *sib = [((i as u8) + 1) * 0x10; 32];
        }
        let prev_hash_fr =
            gosh_dense_balanced_tree::bytes_to_fr(&chain_data.prev_max_level_layer_hash);
        let chain_links = chain_data_to_dense_links(&chain_data);
        let bk_set_hash = Fr::from(0xDEADBEEFu64);

        let circuit = LayerHashesMovementCheckerCircuit::new(
            preimage,
            siblings,
            prev_hash_fr,
            (chain_data.num_prev_chain_steps + 1) as u8,
            chain_links,
            bk_set_hash,
            self.k as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
        );
        let base_params = circuit.base_circuit_params().clone();
        info!("layer base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("layer keygen_vk failed")?;
        info!("layer keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk =
            keygen_pk(&self.srs, vk.clone(), &circuit).context("layer keygen_pk failed")?;
        info!("layer keygen_pk: {:?}", t.elapsed());

        common::save_vk(&self.params_dir, PREFIX, &vk)?;
        common::save_pk(&self.params_dir, PREFIX, &pk)?;
        common::save_config(&self.params_dir, PREFIX, &base_params)?;

        self.vk = Some(vk);
        // pk dropped here — freed ~2.8 GB; reload on demand.
        self.config = Some(base_params);

        info!("layer keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("layer config not loaded — run ensure_keys first")
        })?;
        info!("loading layer PK from disk (~2.8 GB)...");
        let t = std::time::Instant::now();
        let pk = common::try_load_pk(&self.params_dir, PREFIX, config).ok_or_else(|| {
            anyhow::format_err!(
                "failed to load layer PK from {}",
                common::pk_path(&self.params_dir, PREFIX).display()
            )
        })?;
        info!("layer PK loaded in {:?}", t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    pub fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("layer PK unloaded from memory");
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
        self.vk.as_ref().expect("layer VK not loaded")
    }
    pub fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk.as_ref().expect("layer PK not loaded")
    }
    pub fn config(&self) -> &BaseCircuitParams {
        self.config.as_ref().expect("layer config not loaded")
    }

    pub fn num_unusable_rows(&self) -> usize {
        NUM_UNUSABLE_ROWS
    }
    pub fn lookup_bits(&self) -> usize {
        LOOKUP_BITS
    }
}

// ---- Helpers for layer-circuit keygen (layer-specific — kept private to this module) ----

/// Build a 331-byte preimage from `LayerHashChainData` for reference-circuit keygen.
fn build_reference_preimage(chain_data: &LayerHashChainData) -> [u8; LAYER_PREIMAGE_SIZE] {
    let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
    preimage[0] = chain_data.num_layers as u8;
    for i in 0..10 {
        let offset = 1 + i * 33;
        preimage[offset] = (i + 1) as u8;
        if i < chain_data.num_layers {
            preimage[offset + 1..offset + 1 + 32]
                .copy_from_slice(&chain_data.root_hashes[i]);
        }
    }
    preimage
}

/// Convert `LayerHashChainData` chain proofs to `DenseChainLink`s.
fn chain_data_to_dense_links(chain_data: &LayerHashChainData) -> Vec<DenseChainLink> {
    chain_data
        .chain_proofs
        .iter()
        .map(|step| DenseChainLink {
            active: step.active,
            siblings: step.siblings.clone(),
            position: step.position,
            leaf_native: step.leaf_value,
        })
        .collect()
}
