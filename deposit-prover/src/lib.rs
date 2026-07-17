//! Deposit Prover Library
//!
//! This library provides ZK proof generation for Ethereum deposit events.
//! It uses axiom-eth to prove that a specific Deposit event was emitted on
//! Ethereum by verifying the MPT proof and extracting event data.

pub mod aggregation;
pub mod circuit_v2;
pub mod ethereum_fetcher;
pub mod mpt;
pub mod prover;
pub mod rlp_utils;
pub mod synthetic_fixture;
pub mod types;

pub use aggregation::{aggregate_proof, generate_aggregation_verifier, AggregationConfig};
pub use circuit_v2::{DepositEventCircuitV2, MptWitnessMutation};
pub use ethereum_fetcher::EthereumFetcher;
pub use prover::{
    generate_proof, generate_solidity_verifier, load_kzg_params_from_trusted_setup,
    test_circuit_mock, test_circuit_mock_with_mpt_mutation, verify_proof, CircuitConfig,
};
pub use types::{DepositEventData, DepositProofInput, DepositProofOutput, ReceiptProof};
pub use synthetic_fixture::{audit_circuit_config, synthetic_deposit_proof_input};
