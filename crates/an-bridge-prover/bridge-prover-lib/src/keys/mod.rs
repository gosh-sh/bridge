//! Circuit key management for the four bridge halo2 circuits.
//!
//! Prefer the per-circuit managers ([`PrimaryKeyManager`],
//! [`FallbackKeyManager`], [`LayerHashesKeyManager`], [`EventKeyManager`])
//! for new code — each owns its own SRS + VK/PK/config lifecycle and can
//! be constructed independently at any degree via `new_with_k`.
//!
//! The monolithic [`KeyManager`] is retained as a **thin facade** that
//! wires all four together and forwards each old accessor to the right
//! per-circuit manager. It exists so that pre-existing callers keep
//! compiling; new code should reach for the per-circuit types directly.

mod common;
mod state;
pub use common::{assert_hermez_srs, HERMEZ_S_G2_HEAD};
pub mod event;
pub mod fallback;
pub mod layer;
pub mod primary;

pub use event::EventKeyManager;
pub use fallback::FallbackKeyManager;
pub use layer::LayerHashesKeyManager;
pub use primary::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers,
    circuit_num_limbs, circuit_num_unusable_rows, PrimaryKeyManager,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::G1Affine,
    plonk::{ProvingKey, VerifyingKey},
};

/// Facade that owns all four per-circuit managers.
///
/// Prove/verify paths use **per-circuit** SRS accessors
/// (`key_manager.primary.srs()`, `.fallback.srs()`, `.layer.srs()`,
/// `.event.srs()`). halo2-axiom requires `params.n() == 1 << circuit.k()`,
/// so each sub-manager holds its own degree-matched SRS slice loaded via
/// `keys::common::load_srs` (which downsizes from the largest on-disk
/// ceremony when an exact match is not present).
///
/// Callers that need the max-degree (K=21) ceremony to downsize from —
/// e.g. the Poseidon-transcript exporters that provision per-circuit
/// `kzg_bn254_{k}.srs` files on disk — should read `.fallback.srs()`
/// directly.
pub struct KeyManager {
    pub params_dir: PathBuf,
    pub primary: PrimaryKeyManager,
    pub fallback: FallbackKeyManager,
    pub layer: LayerHashesKeyManager,
    pub event: EventKeyManager,
}

impl KeyManager {
    /// Build all four per-circuit managers at their `DEFAULT_K`. Loads
    /// SRS + any cached VK/config off disk; PKs stay on disk.
    pub fn new(params_dir: &Path) -> Self {
        std::fs::create_dir_all(params_dir).ok();
        Self {
            params_dir: params_dir.to_path_buf(),
            primary: PrimaryKeyManager::new(params_dir),
            fallback: FallbackKeyManager::new(params_dir),
            layer: LayerHashesKeyManager::new(params_dir),
            event: EventKeyManager::new(params_dir),
        }
    }

    // ---- Circuit 1a (Primary Attestation) forwarders ----

    pub fn ensure_primary_keys(
        &mut self,
        bk_set: &HashMap<u16, Vec<u8>>,
    ) -> anyhow::Result<()> {
        self.primary.ensure_keys(bk_set)
    }
    pub fn primary_vk(&self) -> &VerifyingKey<G1Affine> {
        self.primary.vk()
    }
    pub fn primary_pk(&self) -> &ProvingKey<G1Affine> {
        self.primary.pk()
    }
    pub fn primary_config(&self) -> &BaseCircuitParams {
        self.primary.config()
    }
    pub fn load_primary_pk(&mut self) -> anyhow::Result<()> {
        self.primary.load_pk()
    }
    pub fn unload_primary_pk(&mut self) {
        self.primary.unload_pk()
    }

    // ---- Circuit 1b (Fallback Attestation) forwarders ----

    pub fn ensure_fallback_keys(
        &mut self,
        bk_set: &HashMap<u16, Vec<u8>>,
    ) -> anyhow::Result<()> {
        self.fallback.ensure_keys(bk_set)
    }
    pub fn fallback_vk(&self) -> &VerifyingKey<G1Affine> {
        self.fallback.vk()
    }
    pub fn fallback_pk(&self) -> &ProvingKey<G1Affine> {
        self.fallback.pk()
    }
    pub fn fallback_config(&self) -> &BaseCircuitParams {
        self.fallback.config()
    }
    pub fn load_fallback_pk(&mut self) -> anyhow::Result<()> {
        self.fallback.load_pk()
    }
    pub fn unload_fallback_pk(&mut self) {
        self.fallback.unload_pk()
    }

    // ---- Circuit 2 (Layer-Hashes Movement) forwarders ----

    pub fn ensure_layer_keys(&mut self) -> anyhow::Result<()> {
        self.layer.ensure_keys()
    }
    pub fn layer_vk(&self) -> &VerifyingKey<G1Affine> {
        self.layer.vk()
    }
    pub fn layer_pk(&self) -> &ProvingKey<G1Affine> {
        self.layer.pk()
    }
    pub fn layer_config(&self) -> &BaseCircuitParams {
        self.layer.config()
    }
    pub fn load_layer_pk(&mut self) -> anyhow::Result<()> {
        self.layer.load_pk()
    }
    pub fn unload_layer_pk(&mut self) {
        self.layer.unload_pk()
    }
    pub fn layer_k(&self) -> usize {
        self.layer.k() as usize
    }
    pub fn layer_num_unusable_rows(&self) -> usize {
        self.layer.num_unusable_rows()
    }
    pub fn layer_lookup_bits(&self) -> usize {
        self.layer.lookup_bits()
    }

    // ---- Circuit 4 (Event Prove — WithdrawalInitiated) forwarders ----

    pub fn ensure_event_keys(&mut self) -> anyhow::Result<()> {
        self.event.ensure_keys()
    }
    pub fn event_vk(&self) -> &VerifyingKey<G1Affine> {
        self.event.vk()
    }
    pub fn event_pk(&self) -> &ProvingKey<G1Affine> {
        self.event.pk()
    }
    pub fn event_config(&self) -> &BaseCircuitParams {
        self.event.config()
    }
    pub fn load_event_pk(&mut self) -> anyhow::Result<()> {
        self.event.load_pk()
    }
    pub fn unload_event_pk(&mut self) {
        self.event.unload_pk()
    }
    pub fn event_k(&self) -> usize {
        self.event.k() as usize
    }
}
