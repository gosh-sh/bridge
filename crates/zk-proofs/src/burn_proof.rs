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
    /// For dummy implementation: contains withdrawal_hash and nullifier preimages
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
///
/// This implementation verifies that the caller knows the preimages of:
/// - withdrawal_hash
/// - nullifier
/// - commitment = hash(withdrawal_hash, nullifier)
#[derive(Debug, Clone)]
pub struct DummyBurnProofProvider {
    /// Whether to accept all proofs (for basic testing)
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
    fn verify_burn(&self, proof: &BurnProof) -> Result<bool> {
        if self.accept_all {
            return Ok(true);
        }

        // For dummy implementation, proof_data should contain:
        // [withdrawal_hash (32 bytes), nullifier (32 bytes), commitment (32 bytes)]
        if proof.proof_data.len() != 96 {
            return Err(ProofError::VerificationFailed(
                format!("Invalid proof data length: expected 96 bytes, got {}", proof.proof_data.len())
            ));
        }

        // Extract preimages
        let withdrawal_hash = Hash::new(
            proof.proof_data[0..32]
                .try_into()
                .map_err(|_| ProofError::VerificationFailed("Invalid withdrawal_hash".to_string()))?
        );
        let nullifier = Hash::new(
            proof.proof_data[32..64]
                .try_into()
                .map_err(|_| ProofError::VerificationFailed("Invalid nullifier".to_string()))?
        );
        let claimed_commitment = Hash::new(
            proof.proof_data[64..96]
                .try_into()
                .map_err(|_| ProofError::VerificationFailed("Invalid commitment".to_string()))?
        );

        // Verify that commitment = hash(withdrawal_hash, nullifier)
        let computed_commitment = crypto::hash_commitment(&withdrawal_hash, &nullifier);

        if computed_commitment != claimed_commitment {
            return Err(ProofError::VerificationFailed(
                "Commitment verification failed: hash(withdrawal_hash, nullifier) != claimed_commitment".to_string()
            ));
        }

        Ok(true)
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

        // Should fail with invalid proof data length
        assert!(provider.verify_burn(&proof).is_err());
    }

    #[test]
    fn test_dummy_provider_verifies_preimages() {
        let provider = DummyBurnProofProvider::new_rejecting();

        // Create valid preimages
        let withdrawal_hash = Hash::new([1u8; 32]);
        let nullifier = Hash::new([2u8; 32]);
        let commitment = crypto::hash_commitment(&withdrawal_hash, &nullifier);

        // Build proof_data: [withdrawal_hash || nullifier || commitment]
        let mut proof_data = Vec::new();
        proof_data.extend_from_slice(withdrawal_hash.as_bytes());
        proof_data.extend_from_slice(nullifier.as_bytes());
        proof_data.extend_from_slice(commitment.as_bytes());

        let proof = BurnProof::new(
            Hash::new([3u8; 32]),
            1000,
            [0u8; 20],
            proof_data,
        );

        // Should succeed because commitment is valid
        assert!(provider.verify_burn(&proof).unwrap());
    }

    #[test]
    fn test_dummy_provider_rejects_invalid_commitment() {
        let provider = DummyBurnProofProvider::new_rejecting();

        // Create preimages
        let withdrawal_hash = Hash::new([1u8; 32]);
        let nullifier = Hash::new([2u8; 32]);
        let wrong_commitment = Hash::new([99u8; 32]); // Wrong commitment

        // Build proof_data with wrong commitment
        let mut proof_data = Vec::new();
        proof_data.extend_from_slice(withdrawal_hash.as_bytes());
        proof_data.extend_from_slice(nullifier.as_bytes());
        proof_data.extend_from_slice(wrong_commitment.as_bytes());

        let proof = BurnProof::new(
            Hash::new([3u8; 32]),
            1000,
            [0u8; 20],
            proof_data,
        );

        // Should fail because commitment doesn't match
        let result = provider.verify_burn(&proof);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Commitment verification failed"));
    }

    #[test]
    fn test_dummy_provider_rejects_invalid_length() {
        let provider = DummyBurnProofProvider::new_rejecting();

        // Create proof_data with wrong length
        let proof_data = vec![1u8; 50]; // Wrong length

        let proof = BurnProof::new(
            Hash::new([3u8; 32]),
            1000,
            [0u8; 20],
            proof_data,
        );

        // Should fail because of invalid length
        let result = provider.verify_burn(&proof);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid proof data length"));
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

