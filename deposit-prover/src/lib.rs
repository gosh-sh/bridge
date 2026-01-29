//! Deposit Prover Library
//!
//! This library provides ZK proof generation for Ethereum deposit events.
//! It proves that a user knows the secrets that created a specific deposit
//! without revealing those secrets.

pub mod circuit;
pub mod circuit_v2;
pub mod ethereum;
pub mod mpt;
pub mod prover;
pub mod rlp_utils;
pub mod types;

pub use circuit::{CircuitConfig, DepositEventCircuit};
pub use types::{DepositEventData, DepositProofInput, DepositProofOutput, ReceiptProof};

