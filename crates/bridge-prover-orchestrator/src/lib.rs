//! Bridge Prover Orchestrator — Phase 1.A
//!
//! This crate wraps the partner's halo2 circuits with key/prover/verifier helpers
//! that the orchestrator and the relayer (planned) will consume.
//!
//! Phase 1.A scope: **Circuit 1B (Fallback attestation verifier)** only.
//! Phases 1.B and 1.C will add Circuit 2 (Layer hashes) and Circuit 3 (BK set update).
//!
//! Re-exports the partner's primary key management for convenience so a single
//! consumer can drive both Circuit 1A and 1B with one set of types.

pub mod bound_test_data;
pub mod keys;
pub mod layer_hashes_keys;
pub mod layer_hashes_prover;
pub mod layer_hashes_test_data;
pub mod proof_export;
pub mod prover;
pub mod verifier;

pub use bridge_prover_lib::Fr;
pub use bridge_prover_lib::keys::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers,
    circuit_num_limbs, circuit_num_unusable_rows,
};
pub use bridge_prover_lib::poseidon::compute_bk_set_poseidon;

pub use keys::FallbackKeyManager;
pub use layer_hashes_keys::{
    LayerHashesKeyManager, LayerHashesReferenceWitness, LAYER_HASHES_K,
    LAYER_HASHES_LOOKUP_BITS, LAYER_HASHES_NUM_UNUSABLE_ROWS,
};
pub use layer_hashes_prover::{
    generate_layer_hashes_proof, verify_layer_hashes_proof, LayerHashesProofInput,
    LayerHashesProofOutput, LAYER_HASHES_NUM_PUBLIC_INPUTS,
};
pub use bound_test_data::{
    build_bound_test_data, compose_layer_hashes_input, promote_bridge_test_data,
    BoundBlockTestData,
};
pub use layer_hashes_test_data::{
    build_synthetic_layer_hashes_input, SyntheticLayerHashesInput,
};
pub use proof_export::{
    build_proof_data, format_field_element, load_instances_binary, save_instances_binary,
    save_proof_data_json, Halo2ProofData, ProtocolData,
};
pub use prover::{generate_fallback_proof, FallbackProofOutput};
pub use verifier::verify_fallback_proof;
