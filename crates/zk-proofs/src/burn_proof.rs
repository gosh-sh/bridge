//! Burn proof verification for Acki Nacki transactions
//!
//! This module provides an abstract interface for verifying that tokens were
//! burned on the Acki Nacki chain. The actual implementation will be provided
//! by the Acki Nacki team.

use crate::error::{ProofError, Result};
use crypto::Hash;
use serde::{Deserialize, Serialize};

/// Proof that tokens were burned on Acki Nacki
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnProof {
    /// Transaction hash on Acki Nacki
    pub tx_hash: Hash,
    /// Amount burned
    pub amount: u64,
    /// Recipient address on Ethereum
    pub recipient: [u8; 20],
    /// Additional proof data (format TBD by Acki Nacki team)
    pub proof_data: Vec<u8>,
}

impl BurnProof {
    /// Create a new burn proof
    pub fn new(tx_hash: Hash, amount: u64, recipient: [u8; 20], proof_data: Vec<u8>) -> Self {
        Self {
            tx_hash,
            amount,
            recipient,
            proof_data,
        }
    }

    /// Get transaction hash
    pub fn tx_hash(&self) -> &Hash {
        &self.tx_hash
    }

    /// Get amount
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// Get recipient
    pub fn recipient(&self) -> &[u8; 20] {
        &self.recipient
    }
}

/// Trait for providing burn proofs
///
/// This trait abstracts the burn proof verification logic, allowing for
/// different implementations (real Acki Nacki integration, mocks for testing, etc.)
pub trait BurnProofProvider: Send + Sync {
    /// Verify a burn proof
    fn verify_burn(&self, proof: &BurnProof) -> Result<bool>;

    /// Get burn proof for a transaction
    fn get_burn_proof(&self, tx_hash: &Hash) -> Result<BurnProof>;
}

/// Dummy burn proof provider for testing
#[derive(Debug, Clone)]
pub struct DummyBurnProofProvider {
    /// Whether to accept all proofs
    accept_all: bool,
}

impl DummyBurnProofProvider {
    /// Create a new dummy provider that accepts all proofs
    pub fn new_accepting() -> Self {
        Self { accept_all: true }
    }

    /// Create a new dummy provider that rejects all proofs
    pub fn new_rejecting() -> Self {
        Self { accept_all: false }
    }
}

impl BurnProofProvider for DummyBurnProofProvider {
    fn verify_burn(&self, _proof: &BurnProof) -> Result<bool> {
        Ok(self.accept_all)
    }

    fn get_burn_proof(&self, tx_hash: &Hash) -> Result<BurnProof> {
        if self.accept_all {
            Ok(BurnProof::new(
                *tx_hash,
                1000, // dummy amount
                [0u8; 20], // dummy recipient
                vec![0xde, 0xad, 0xbe, 0xef], // dummy proof data
            ))
        } else {
            Err(ProofError::VerificationFailed(
                "Dummy provider rejects all proofs".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_burn_proof_creation() {
        let tx_hash = Hash::new([1u8; 32]);
        let amount = 1000u64;
        let recipient = [2u8; 20];
        let proof_data = vec![1, 2, 3, 4];

        let proof = BurnProof::new(tx_hash, amount, recipient, proof_data.clone());

        assert_eq!(proof.tx_hash(), &tx_hash);
        assert_eq!(proof.amount(), amount);
        assert_eq!(proof.recipient(), &recipient);
        assert_eq!(proof.proof_data, proof_data);
    }

    #[test]
    fn test_dummy_provider_accepting() {
        let provider = DummyBurnProofProvider::new_accepting();
        let proof = BurnProof::new(Hash::new([1u8; 32]), 1000, [0u8; 20], vec![]);

        assert!(provider.verify_burn(&proof).unwrap());
    }

    #[test]
    fn test_dummy_provider_rejecting() {
        let provider = DummyBurnProofProvider::new_rejecting();
        let proof = BurnProof::new(Hash::new([1u8; 32]), 1000, [0u8; 20], vec![]);

        assert!(!provider.verify_burn(&proof).unwrap());
    }

    #[test]
    fn test_burn_proof_serialization() {
        let proof = BurnProof::new(Hash::new([1u8; 32]), 1000, [2u8; 20], vec![1, 2, 3]);

        let serialized = serde_json::to_string(&proof).unwrap();
        let deserialized: BurnProof = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.tx_hash(), proof.tx_hash());
        assert_eq!(deserialized.amount(), proof.amount());
        assert_eq!(deserialized.recipient(), proof.recipient());
    }
}

