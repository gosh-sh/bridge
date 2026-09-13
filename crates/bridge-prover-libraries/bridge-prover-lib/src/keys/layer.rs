//! Circuit 2 (Layer-Hashes Movement) key manager.
//!
//! Independent from Circuit 1's shape — smaller K, tighter lookup width,
//! and its own reference-witness path built from
//! `bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth`.
//!
//! Lifecycle (SRS load, cache-hit check, keygen timing/logging, save-and-set,
//! on-demand PK load/unload, accessor plumbing) is delegated to the shared
//! [`super::state::KeyManagerState`]. Only the circuit-specific
//! reference-witness construction and the extra
//! [`LayerHashesKeyManager::num_unusable_rows`] / [`LayerHashesKeyManager::lookup_bits`]
//! accessors live here.

use std::path::Path;

use bridge_test_data_gen::layer_hashes::LayerHashChainData;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit, LAYER_PREIMAGE_SIZE, NUM_MERKLE_SIBLINGS,
};
use tracing::info;

use super::state::KeyManagerState;

pub(super) const PREFIX: &str = "layer";

pub(super) const NUM_UNUSABLE_ROWS: usize = 109;
pub(super) const LOOKUP_BITS: usize = 16;

pub struct LayerHashesKeyManager {
    state: KeyManagerState,
}

impl LayerHashesKeyManager {
    /// Circuit-shape degree (rows / advice layout). Saved in
    /// `layer_config_params.json` and passed into
    /// `LayerHashesMovementCheckerCircuit::new`.
    pub const DEFAULT_K: u32 = 17;

    /// SRS degree used at keygen / prove / verify.
    ///
    /// halo2-axiom's `keygen_vk` sizes `vk.domain` from `params.k()`, **not**
    /// from the circuit's logical k. Partner PKs were keygen'd against the
    /// shared K=20 ceremony, so `pk.domain.k() == 20` even though
    /// `config.k == 17`. Loading an SRS at [`Self::DEFAULT_K`] makes
    /// `create_proof` panic (`domain.n()=2^20` vs `params.n()=2^17`).
    pub const KEYGEN_SRS_K: u32 = 20;

    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

    pub fn new_with_k(params_dir: &Path, k: u32) -> Self {
        // SRS must match the degree baked into cached PKs (see KEYGEN_SRS_K).
        let srs_k = Self::KEYGEN_SRS_K.max(k);
        Self {
            // `None`: this circuit does not version its keys. When it
            // grows a manifest this becomes `Some(..)` and it inherits
            // the whole mechanism.
            state: KeyManagerState::new(params_dir, PREFIX, k, srs_k, None),
        }
    }

    /// Ensure Circuit 2 keys exist. Runs keygen with a synthetic chain
    /// witness — the circuit shape is fixed (MAX_LAYERS / MAX_CHAIN_LEN,
    /// masked in-circuit) so any admissible witness drives keygen the same.
    ///
    /// REF_TREE_DEPTH is derived from the shared
    /// [`crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE`] so it stays in
    /// sync with the on-chain tree cadence.
    pub fn ensure_keys(&mut self) -> anyhow::Result<()> {
        if self.state.keys_cached() {
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
            self.state.k() as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
        );
        let base_params = circuit.base_circuit_params().clone();

        self.state.run_keygen(&circuit, base_params)?;
        info!("layer keys generated and cached (PK on disk, not in memory)");
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
