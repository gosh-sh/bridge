//! Zero-Knowledge Proof System for the Bridge
//!
//! This crate implements Halo2-based ZK proofs for:
//! - Deposit proofs: Prove knowledge of withdrawal hash preimage and amount (legacy)
//! - Withdrawal proofs: Prove knowledge of nullifier, Merkle proof, and burn transaction
//!
//! # Architecture
//!
//! The proof system uses Halo2 (PLONK-based) for efficient proof generation and verification.
//! This crate uses scroll-tech's halo2-lib for compatibility with Acki Nacki.
//!
//! For Ethereum deposit event proofs, see the separate `deposit-prover` binary which uses
//! axiom-eth library (requires axiom-crypto's halo2-lib).
//!
//! # Note
//!
//! The current implementation uses placeholder proofs for initial development.
//! Full Halo2 circuit implementation will be added in production.

pub mod deposit;
pub mod withdrawal;
pub mod burn_proof;
pub mod error;
pub mod types;
pub mod circuits;

pub use deposit::{DepositProof, DepositProver, DepositVerifier};
pub use withdrawal::{WithdrawalProof, WithdrawalProver, WithdrawalVerifier};
pub use burn_proof::{BurnProof, BurnProofProvider, DummyBurnProofProvider};
pub use error::{ProofError, Result};
pub use types::{ProofData, VerificationKey, ProvingKey};

#[cfg(test)]
mod tests;

