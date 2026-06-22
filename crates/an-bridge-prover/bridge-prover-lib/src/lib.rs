pub mod gql_client;
pub mod attestation_fetcher;
pub mod bk_set_fetcher;
pub mod poseidon;
pub mod poseidon_dense;
pub mod types;
pub mod keys;
pub mod prover;
pub mod verifier;
pub mod ipc;
pub mod bridge_state;
pub mod prover_bk_set;
pub mod bootstrap;
pub mod block_id_tree;
pub mod chain_proof_builder;
pub mod real_chain_builder;
pub mod layer_prover;

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
/// Current test config: `W = 128`, `P = 4`. 
pub const THINNING_FACTOR_P: u64 = 4;
