//! Bridge Prover Orchestrator — SHPLONK aggregator pipeline glue.
//!
//! Key management, proof generation, verification, and Fiat–Shamir
//! transcripts are owned by `bridge-prover-lib` + `bridge-event-prover-lib`
//! (the source of truth). This crate is now a thin pipeline layer on top of
//! them, keeping only the concerns that don't overlap with the prover libs:
//!
//! - [`bound_test_data`] — cross-circuit bound-block fixtures for the R15
//!   export pipeline.
//! - [`halo2_snark`] — snark-verifier `Snark` export helper (the gosh↔axiom
//!   boundary for the aggregator).
//! - [`halo2_tvm_bundle`] — the on-chain `ZKHALO2VERIFYWITHVK` `VkBlob` wire
//!   format.
//! - [`proof_export`] — proof/instance serialisation helpers.

pub mod bound_test_data;
pub mod halo2_snark;
pub mod halo2_tvm_bundle;
pub mod proof_export;

pub use bound_test_data::{
    build_bound_test_data, compose_layer_hashes_input, load_bound_witness_cache,
    promote_bridge_test_data, save_bound_witness_cache, BoundBlockTestData,
};
pub use bridge_poseidon::compute_bk_set_poseidon;
pub use bridge_prover_lib::{
    keys::{
        circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers, circuit_num_limbs,
        circuit_num_unusable_rows,
    },
    layer_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS,
    Fr,
};
pub use halo2_tvm_bundle::{
    decode_instances, encode_instances, CircuitShape, Halo2TvmOperands, TranscriptKind, VkBlob,
    VkConfig, VK_BLOB_MAGIC, VK_BLOB_VERSION, VK_BLOB_VERSION_V2,
};
pub use proof_export::{
    build_proof_data, format_field_element, load_instances_binary, save_instances_binary,
    save_proof_data_json, Halo2ProofData, ProtocolData,
};
