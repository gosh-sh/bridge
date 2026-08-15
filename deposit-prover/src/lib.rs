//! Deposit Prover Library
//!
//! This library provides ZK proof generation for Ethereum deposit events.
//! It uses axiom-eth to prove that a specific Deposit event was emitted on
//! Ethereum by verifying the MPT proof and extracting event data.

pub mod aggregation;
pub mod circuit_v2;
pub mod ethereum_fetcher;
pub mod halo2_tvm_bundle;
pub mod mpt;
pub mod opcode_triple_verify;
pub mod prover;
pub mod rlp_utils;
pub mod supported_chains;
pub mod synthetic_fixture;
pub mod types;

pub use aggregation::{aggregate_proof, generate_aggregation_verifier, AggregationConfig};
pub use circuit_v2::{
    BLOCK_HEADER_MAX_FIELD_LENS, DepositEventCircuitV2, DepositWitnessMutation,
    MAX_BLOCK_HEADER_BYTES, MptWitnessMutation,
};
pub use ethereum_fetcher::EthereumFetcher;
pub use opcode_triple_verify::{
    export_blake2b_deposit_triple, verify_deposit_opcode_triple,
};
pub use prover::{
    generate_proof, generate_solidity_verifier, load_kzg_params_from_trusted_setup,
    test_circuit_mock, test_circuit_mock_instances, test_circuit_mock_with_mpt_mutation,
    test_circuit_mock_with_pi_corruption, test_circuit_mock_with_witness_mutation, verify_proof, CircuitConfig,
};
pub use supported_chains::{
    require_supported_deposit_chain, supported_deposit_chain_name, SUPPORTED_DEPOSIT_CHAIN_IDS,
};
pub use types::{
    DepositEventData, DepositProofInput, DepositProofOutput, ReceiptProof, TransactionProof,
    NUM_PUBLIC_INPUTS,
};
pub use rlp_utils::{
    encode_block_header, encode_receipt, encode_tx_index, rlp_header_field_count,
    verify_block_header_rlp, HEADER_SHAPE_SAMPLES, ChainHeaderCorpusEntry,
    CHAIN_HEADER_CORPUS, HeaderShapeSample,
};
pub use synthetic_fixture::{
    audit_circuit_config, production_capacity_config, synthetic_deposit_proof_input,
    synthetic_deposit_proof_input_with_workchain, SYNTHETIC_CHAIN_ID,
};
