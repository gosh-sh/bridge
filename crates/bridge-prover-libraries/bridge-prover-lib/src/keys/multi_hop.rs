//! Cross-thread hop-chain (`BridgeMultiHopProof`) key manager.
//!
//! Same on-disk / on-demand-PK-load pattern as [`super::event::EventKeyManager`]
//! — the two managers together drive the multi-thread Circuit 4 bundle: one
//! `BridgeEventFinalProof` snark plus 0..=`N_BUNDLE_MAX` `BridgeMultiHopProof`
//! snarks (see `MULTITHREAD_MIGRATION_PLAN.md` §5–§6).
//!
//! Uses the deterministic synthetic-witness path from
//! `bridge_event_prove_circuit::test_helpers::build_synthetic_multi_hop_keygen_inputs`;
//! the produced circuit's constraint system is independent of witness
//! values, so the VK/PK shape is stable across machines and CI runs.

use std::path::Path;

use bridge_event_prove_circuit::test_helpers::build_synthetic_multi_hop_keygen_inputs;
use halo2_base::{
    gates::circuit::BaseCircuitParams,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, G1Affine},
        plonk::{ProvingKey, VerifyingKey},
        poly::kzg::commitment::ParamsKZG,
    },
};
use tracing::info;

use super::state::KeyManagerState;

pub(super) const PREFIX: &str = "multi_hop";

/// Bumped **by hand** whenever the `BridgeMultiHopProof` definition changes
/// in a way that invalidates cached keys. Same discipline as
/// [`super::event::EVENT_CIRCUIT_REVISION`]: edit the circuit, bump this in
/// the same PR.
///
/// Rev 2: `KEYGEN_SRS_K` dropped 20 → 17 to match the circuit's actual K.
/// Old K=20 PKs (26 GiB) on disk are invalidated and force fresh keygen at
/// K=17 (~3–4 GiB). See `KEYGEN_SRS_K` doc-comment for the rationale.
///
/// Rev 3: `H_HOPS_PER_PROOF` dropped 5 → 2 and `num_advice_per_phase` dropped
/// 200 → 50 to shrink the inner VK so the outer SHPLONK aggregator fits in a
/// 16 GB workstation. Cuts total SHA-256 compressions per snark from 40 to
/// 16 and the inner column budget by ~4× (see
/// `bridge-circuits/docs/SHA256_INVOCATIONS.md` §4). Breaking change: old
/// PK/VK on disk are invalidated; the outer verifier Yul must be regenerated
/// and the on-chain `BridgeMultiHopAggregatorVerifier` redeployed.
///
/// Rev 4: `H_HOPS_PER_PROOF` dropped 2 → 1 and `num_advice_per_phase` dropped
/// 50 → 25 so the outer SHPLONK YUL verifier fits under EIP-170 (24 576 B).
/// At H=2 the outer bytecode came out to 33 213 B (135 %, FAIL). At H=1 the
/// inner circuit is SHA-dominated (~2.83 M cells for 8 compressions) and
/// matches the shape of
/// `historical-layer-hashes-movement-checker-circuit` — predicted outer YUL
/// ≈ 21 KB at `k_outer=21, Full`, no Hermez K=22 SRS bootstrap needed. See
/// `bridge-circuits/docs/CIRCUIT_COMPLEXITY_COMPARISON.md` §§2, 5. Breaking
/// change: old PK/VK on disk are invalidated, `N_BUNDLE_MAX` doubles from 10
/// to 20, the outer verifier YUL must be regenerated and the on-chain
/// `BridgeMultiHopAggregatorVerifier` redeployed.
///
/// Rev 5: `ref_index = 0` (the same-thread `parent_block_id` edge of the L7
/// Poseidon dense-Merkle tree) is now a valid hop edge. The circuit selects
/// the Poseidon leaf tag per hop on `is_zero(ref_index)`:
/// `REFERENCED_PARENT_BLOCK_TAG` (37 B, chunks 31+31+7) vs
/// `REFERENCED_REF_BLOCK_TAG` (34 B, chunks 31+31+4). This adds a per-hop
/// `gate.select` over the three Fr chunk inputs plus two constants
/// (`pow_256_3`, `pow_256_6`) and two tag-chunk constant pairs; the hard
/// `assert_ref_index_is_cross_thread` ban is dropped. See
/// `bridge/multithreading/README.md` §2.0 for the chain-side rationale.
/// Breaking change: old PK/VK on disk are invalidated. `BaseCircuitParams`
/// are unchanged (K=17, 25 advice, 1 instance column), so the universal
/// outer SHPLONK aggregator stays byte-identical:
/// `BridgeMultiHopAggregatorVerifier.{sol,bin}` do not change and no
/// on-chain redeploy is required. The in-tree
/// `BridgeMultiHopAggregatorVerifier_calldata.bin` fixture is regenerated
/// against the rotated inner VK.
pub(super) const MULTI_HOP_CIRCUIT_REVISION: u32 = 5;

/// Deterministic seed for the synthetic-witness keygen path. Any seed
/// produces the same VK/PK shape.
pub(super) const MULTI_HOP_KEYGEN_SEED: u64 = 0x1B0D_1B0D_1B0D_1B0Du64;

pub struct MultiHopKeyManager {
    state: KeyManagerState,
}

impl MultiHopKeyManager {
    /// Circuit-shape degree — matches
    /// `bridge_event_prove_circuit::test_helpers::MULTI_HOP_K` and the `k`
    /// field of `multi_hop_base_circuit_params()`.
    pub const DEFAULT_K: u32 = 17;

    /// SRS degree used at keygen / prove / verify. Matches the circuit's
    /// arithmetic K so `vk.domain.k == 17` — halo2-axiom bakes `params.k()`
    /// into `vk.domain`, so an oversized SRS (K=20) would produce a
    /// K=20-domain PK (~26 GiB) even though the circuit only uses 2^17 rows.
    ///
    /// Unlike `EventKeyManager::KEYGEN_SRS_K` (K=20 for partner-PK legacy
    /// compat), multi-hop is a brand-new circuit with no legacy PK to match,
    /// and the outer SHPLONK aggregator (`bridge-evm-aggregator`) reads inner
    /// domain from the VK at compile time — no inner-K pinning either.
    ///
    /// K=17 provisioning still shares the K=20 ceremony's `g2`/`s_g2`: the
    /// export bin downsizes from the K=21 fallback SRS (see
    /// `ensure_srs_for_multi_hop`), and downsize preserves the pairing basis.
    pub const KEYGEN_SRS_K: u32 = 17;

    pub fn new(params_dir: &Path) -> Self {
        Self::new_with_k(params_dir, Self::DEFAULT_K)
    }

    pub fn new_with_k(params_dir: &Path, k: u32) -> Self {
        let srs_k = Self::KEYGEN_SRS_K.max(k);
        Self {
            state: KeyManagerState::new(
                params_dir,
                PREFIX,
                k,
                srs_k,
                Some(MULTI_HOP_CIRCUIT_REVISION),
            ),
        }
    }

    pub fn ensure_keys(&mut self) -> anyhow::Result<()> {
        if self.state.keys_cached() {
            info!("multi-hop keys already available (VK in memory, PK on disk)");
            return Ok(());
        }
        info!("running keygen for multi-hop circuit (this may take a while)...");

        let (circuit, _instances) = build_synthetic_multi_hop_keygen_inputs(MULTI_HOP_KEYGEN_SEED);
        let base_params = circuit.base_circuit_params.clone();

        self.state.run_keygen(&circuit, base_params)?;
        info!("multi-hop keys generated and cached (PK on disk, not in memory)");
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
}
