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
    halo2curves::bn256::{Bn256, G1Affine},
    plonk::{ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};

/// Facade that owns all four per-circuit managers plus a shared SRS sized
/// at the **maximum** circuit degree across the four circuits.
///
/// The `pub srs` field is the SRS used by every shared prover/verifier path
/// (`crate::prover`, `crate::verifier`, `crate::layer_prover` all call
/// `create_proof`/`SingleStrategy::new` against `&key_manager.srs`).
/// Orchestrator + daemon code depend on this field name staying stable.
///
/// It is sized at `FallbackKeyManager::DEFAULT_K` (= 21), the largest degree
/// any of the four circuits uses (Primary 20, Fallback **21**, Layer 17,
/// Event 19). A larger-K KZG SRS proves/verifies every smaller-K circuit, so
/// one SRS covers all four. Sizing it at K=20 (the old primary default) left
/// `generate_fallback_proof(&KeyManager, ..)` proving the K=21 fallback
/// circuit against a too-small K=20 SRS — the facade could never produce a
/// valid fallback proof (see AB-Q5 review, 2026-07-06).
pub struct KeyManager {
    pub params_dir: PathBuf,
    /// Max-degree (K=21) SRS shared by all four circuits' prove/verify paths.
    /// A KZG SRS at the max K proves/verifies every smaller-K circuit, so
    /// Primary (K=20), Layer (K=17) and Event (K=19) reuse it directly.
    pub srs: ParamsKZG<Bn256>,
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
        // Size the shared SRS at the MAX circuit degree (Fallback's K=21), not
        // the primary K=20: a larger-K KZG SRS proves/verifies every smaller-K
        // circuit, and the shared `generate_fallback_proof(&KeyManager, ..)`
        // path proves the K=21 fallback circuit against this SRS. See AB-Q5.
        let srs = common::load_srs(params_dir, FallbackKeyManager::DEFAULT_K);
        Self {
            params_dir: params_dir.to_path_buf(),
            srs,
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
