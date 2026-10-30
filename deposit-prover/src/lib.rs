//! Deposit Prover Library
//!
//! This library provides ZK proof generation for Ethereum deposit events.
//! It uses axiom-eth to check a Deposit log against an MPT proof and a block
//! header and to extract the event data. Its keccak results are promise outputs
//! the prover supplies, so a proof does not show the event was emitted on
//! Ethereum (see `README.md`).

pub mod aggregation;
pub mod circuit_v2;
pub mod ethereum_fetcher;
pub mod halo2_tvm_bundle;
pub mod mpt;
pub mod provable;
pub mod prover;
pub mod rlp_utils;
pub mod supported_chains;
pub mod synthetic;
pub mod types;

pub use aggregation::{aggregate_proof, generate_aggregation_verifier, AggregationConfig};
pub use circuit_v2::DepositEventCircuitV2;
pub use ethereum_fetcher::EthereumFetcher;
pub use prover::{
    generate_proof, generate_solidity_verifier, load_kzg_params_from_trusted_setup,
    test_circuit_mock, test_circuit_mock_pinned, verify_proof, CircuitConfig,
};
pub use supported_chains::{
    require_supported_deposit_chain, supported_deposit_chain_name, SUPPORTED_DEPOSIT_CHAIN_IDS,
};
pub use types::{
    DepositEventData, DepositProofInput, DepositProofOutput, ReceiptProof, TransactionProof,
    NUM_PUBLIC_INPUTS,
};
