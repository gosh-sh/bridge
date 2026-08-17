pub mod poseidon_dense;
pub mod keys;
pub mod transcript;
pub mod prover;
pub mod verifier;
pub mod ipc;
pub mod bridge_state;
pub mod prover_bk_set;
pub mod bootstrap;
pub mod bk_set_bootstrap;
pub mod block_id_tree;
pub mod chain_proof_builder;
pub mod real_chain_builder;
pub mod layer_prover;
pub mod live_driver;

// Re-export commonly used types.
pub use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

/// Prover-side thinning factor `P`: the prover only emits a (Circuit 1 + Circuit 2)
/// bundle every `P`-th master key block instead of every key block. See
/// `acki-nacki-to-eth-bridge-halo2-circuits/BRIDGE_PROVER_THINNING_SPEC.md`.
///
/// Hard constraints (checked at runtime by `chain_proof_builder::build_chain_proofs`):
///   * `P <= MAX_CHAIN_LEN = 11` (from `gosh-dense-balanced-tree`)
///   * `P` must divide `W = crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE`
///     so the on-chain `layerWindows[L≥2]` cadence is unchanged.
///
/// Current config: `W = 128`, `P = 8` (bundle stride `W·P = 1024`).
///
/// Re-applied 2026-08-17 as prep for Deploy #7 on Sepolia. Earlier same-day
/// attempt was reverted because live Deploy #6's on-chain
/// `expectedPrevAnchor(1)` was fossilized under P=4 arithmetic and a
/// cold-restart at P=8 would have reverted the first `verifyBlock` with
/// `PrevAnchorMismatch`. There's no in-place P transition on a live bridge —
/// a P bump requires a fresh deploy at the new P. Deploy #7 is a fresh
/// bridge at fresh W·P=1024-aligned anchors, so this constant is now
/// consistent with the on-chain genesis stamp again.
///
/// Cadence at P=8 / shellnet 3 b/s: 1024 seq_nos / bundle → 341 s chain-time
/// vs ~597 s prover-time (warm cache). Prover still ~1.75× slower than
/// chain; not real-time sustainable but survives long enough for a single
/// withdrawal E2E from a fresh-head seed. Spec §1.2 notes P=8 is the
/// recommended setting within the `MAX_CHAIN_LEN=11 ∧ W=128` ceiling;
/// steady-state throughput requires a producer-side W bump or faster HW.
pub const THINNING_FACTOR_P: u64 = 8;
