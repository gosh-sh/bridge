//! Withdrawal proof circuit and prover
//!
//! The withdrawal proof demonstrates knowledge of:
//! - Preimage of the withdrawal hash
//! - The nullifier
//! - Merkle proof of inclusion in the tree
//! - Burn transaction on Acki Nacki
//!
//! Public inputs:
//! - Amount
//! - Merkle root

use crate::burn_proof::BurnProof;
use crate::error::{ProofError, Result};
use crate::types::{ProofData, ProvingKey, VerificationKey};
use crypto::Hash;
use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    halo2curves::bn256::{Bn256, Fr},
    plonk::{keygen_pk, keygen_vk, Circuit, ConstraintSystem, Error},
    poly::kzg::commitment::ParamsKZG,
};
use merkle_tree::MerkleProof;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

/// Withdrawal proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalProof {
    /// Proof data
    pub proof: ProofData,
    /// Amount (public input)
    pub amount: u64,
    /// Merkle root (public input)
    pub merkle_root: Hash,
    /// Nullifier (to prevent double spending)
    pub nullifier: Hash,
}

impl WithdrawalProof {
    /// Create a new withdrawal proof
    pub fn new(proof: ProofData, amount: u64, merkle_root: Hash, nullifier: Hash) -> Self {
        Self {
            proof,
            amount,
            merkle_root,
            nullifier,
        }
    }

    /// Get amount
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// Get Merkle root
    pub fn merkle_root(&self) -> &Hash {
        &self.merkle_root
    }

    /// Get nullifier
    pub fn nullifier(&self) -> &Hash {
        &self.nullifier
    }
}

/// Withdrawal circuit configuration
#[derive(Clone, Debug)]
pub struct WithdrawalCircuitConfig {
    /// Number of advice columns
    pub num_advice: usize,
    /// Number of lookup advice columns
    pub num_lookup_advice: usize,
    /// Number of fixed columns
    pub num_fixed: usize,
    /// Lookup bits
    pub lookup_bits: usize,
}

impl Default for WithdrawalCircuitConfig {
    fn default() -> Self {
        Self {
            num_advice: 1,
            num_lookup_advice: 1,
            num_fixed: 1,
            lookup_bits: 8,
        }
    }
}

/// Withdrawal circuit
#[derive(Clone, Debug)]
pub struct WithdrawalCircuit {
    /// Withdrawal hash (private input)
    pub withdrawal_hash: Value<Fr>,
    /// Nullifier (private input)
    pub nullifier: Value<Fr>,
    /// Merkle root (public input)
    pub merkle_root: Value<Fr>,
    /// Amount (public input)
    pub amount: Value<Fr>,
    /// Circuit configuration
    pub config: WithdrawalCircuitConfig,
}

impl WithdrawalCircuit {
    /// Create a new withdrawal circuit
    pub fn new(
        withdrawal_hash: Value<Fr>,
        nullifier: Value<Fr>,
        merkle_root: Value<Fr>,
        amount: Value<Fr>,
        config: WithdrawalCircuitConfig,
    ) -> Self {
        Self {
            withdrawal_hash,
            nullifier,
            merkle_root,
            amount,
            config,
        }
    }
}

// Implement Circuit trait for WithdrawalCircuit
impl Circuit<Fr> for WithdrawalCircuit {
    type Config = ();
    type FloorPlanner = SimpleFloorPlanner;
    #[cfg(feature = "circuit-params")]
    type Params = ();

    fn without_witnesses(&self) -> Self {
        Self {
            withdrawal_hash: Value::unknown(),
            nullifier: Value::unknown(),
            merkle_root: Value::unknown(),
            amount: Value::unknown(),
            config: self.config.clone(),
        }
    }

    fn configure(_meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        // Configuration is handled by halo2-base internally
        ()
    }

    fn synthesize(
        &self,
        _config: Self::Config,
        mut _layouter: impl Layouter<Fr>,
    ) -> std::result::Result<(), Error> {
        // TODO: Implement proper circuit synthesis using halo2-base
        // This requires:
        // 1. Create BaseCircuitBuilder
        // 2. Assign witness values (withdrawal_hash, nullifier)
        // 3. Compute commitment = Poseidon(withdrawal_hash, nullifier)
        // 4. Verify Merkle proof of commitment inclusion
        // 5. Verify burn proof
        // 6. Expose amount and merkle_root as public inputs

        // For now, this is a placeholder
        Ok(())
    }
}

/// Withdrawal prover
pub struct WithdrawalProver {
    k: u32,
    params: ParamsKZG<Bn256>,
    config: WithdrawalCircuitConfig,
}

impl WithdrawalProver {
    /// Create a new withdrawal prover
    pub fn new(k: u32) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self {
            k,
            params,
            config: WithdrawalCircuitConfig::default(),
        }
    }

    /// Create with custom configuration
    pub fn with_config(k: u32, config: WithdrawalCircuitConfig) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self { k, params, config }
    }

    /// Generate proving and verification keys
    pub fn keygen(&self) -> Result<(ProvingKey, VerificationKey)> {
        // Create empty circuit for keygen
        let circuit = WithdrawalCircuit::new(
            Value::unknown(),
            Value::unknown(),
            Value::unknown(),
            Value::unknown(),
            self.config.clone(),
        );

        let vk = keygen_vk(&self.params, &circuit)
            .map_err(|e| ProofError::GenerationFailed(format!("VK generation failed: {:?}", e)))?;

        let _pk = keygen_pk(&self.params, vk.clone(), &circuit)
            .map_err(|e| ProofError::GenerationFailed(format!("PK generation failed: {:?}", e)))?;

        // Serialize keys
        // TODO: Implement proper serialization for keys
        let vk_bytes = vec![]; // Placeholder
        let pk_bytes = vec![]; // Placeholder

        Ok((
            ProvingKey::from_bytes(pk_bytes),
            VerificationKey::from_bytes(vk_bytes),
        ))
    }

    /// Generate a withdrawal proof
    pub fn prove(
        &self,
        _pk: &ProvingKey,
        withdrawal_hash: &Hash,
        nullifier: &Hash,
        merkle_proof: &MerkleProof,
        _burn_proof: &BurnProof,
        amount: u64,
    ) -> Result<WithdrawalProof> {
        // Verify Merkle proof first
        if !merkle_proof.verify() {
            return Err(ProofError::InvalidWitness(
                "Invalid Merkle proof".to_string(),
            ));
        }

        // Convert inputs to field elements
        let withdrawal_hash_fe = withdrawal_hash.to_field_element();
        let nullifier_fe = nullifier.to_field_element();
        let merkle_root_fe = merkle_proof.root.to_field_element();
        let amount_fe = Fr::from(amount);

        // Create circuit with witness values
        let _circuit = WithdrawalCircuit::new(
            Value::known(withdrawal_hash_fe),
            Value::known(nullifier_fe),
            Value::known(merkle_root_fe),
            Value::known(amount_fe),
            self.config.clone(),
        );

        // TODO: Implement actual proof generation
        // For now, return placeholder proof
        let proof_data = ProofData::new(
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![amount.to_le_bytes().to_vec(), merkle_proof.root.as_bytes().to_vec()],
        );

        Ok(WithdrawalProof::new(
            proof_data,
            amount,
            merkle_proof.root,
            *nullifier,
        ))
    }
}

/// Withdrawal verifier
pub struct WithdrawalVerifier {
    k: u32,
    params: ParamsKZG<Bn256>,
}

impl WithdrawalVerifier {
    /// Create a new withdrawal verifier
    pub fn new(k: u32) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self { k, params }
    }

    /// Verify a withdrawal proof
    pub fn verify(&self, _vk: &VerificationKey, proof: &WithdrawalProof) -> Result<bool> {
        // TODO: Implement actual verification
        // For now, just check that proof data exists
        Ok(!proof.proof.proof_bytes().is_empty())
    }

    /// Verify withdrawal proof with nullifier check
    pub fn verify_with_nullifier_check(
        &self,
        vk: &VerificationKey,
        proof: &WithdrawalProof,
        used_nullifiers: &std::collections::HashSet<Hash>,
    ) -> Result<bool> {
        // Check if nullifier was already used
        if used_nullifiers.contains(&proof.nullifier) {
            return Err(ProofError::VerificationFailed(
                "Nullifier already used (double spend attempt)".to_string(),
            ));
        }

        // Verify the proof
        self.verify(vk, proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto::SecureRng;
    use merkle_tree::MerkleTree;
    use crate::burn_proof::BurnProof;

    #[test]
    fn test_withdrawal_proof_creation() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let nullifier = Hash::new(rng.random_bytes::<32>());

        // Create a Merkle tree and proof
        let mut tree = MerkleTree::new();
        let commitment = crypto::hash_commitment(&withdrawal_hash, &nullifier);
        let index = tree.insert(commitment).unwrap();
        let merkle_proof = tree.prove(index).unwrap();

        // Create burn proof
        let burn_proof = BurnProof::new(
            Hash::new(rng.random_bytes::<32>()),
            1000,
            [0u8; 20],
            vec![],
        );

        let prover = WithdrawalProver::new(4);
        let proof = prover.prove(
            &ProvingKey::from_bytes(vec![]),
            &withdrawal_hash,
            &nullifier,
            &merkle_proof,
            &burn_proof,
            1000,
        );

        assert!(proof.is_ok());
        let proof = proof.unwrap();
        assert_eq!(proof.amount(), 1000);
        assert_eq!(proof.nullifier(), &nullifier);
    }

    #[test]
    fn test_withdrawal_proof_verification() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let nullifier = Hash::new(rng.random_bytes::<32>());

        let mut tree = MerkleTree::new();
        let commitment = crypto::hash_commitment(&withdrawal_hash, &nullifier);
        let index = tree.insert(commitment).unwrap();
        let merkle_proof = tree.prove(index).unwrap();

        let burn_proof = BurnProof::new(
            Hash::new(rng.random_bytes::<32>()),
            1000,
            [0u8; 20],
            vec![],
        );

        let prover = WithdrawalProver::new(4);
        let proof = prover
            .prove(
                &ProvingKey::from_bytes(vec![]),
                &withdrawal_hash,
                &nullifier,
                &merkle_proof,
                &burn_proof,
                1000,
            )
            .unwrap();

        let verifier = WithdrawalVerifier::new(4);
        let result = verifier.verify(&VerificationKey::from_bytes(vec![]), &proof);

        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_nullifier_double_spend_prevention() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let nullifier = Hash::new(rng.random_bytes::<32>());

        let mut tree = MerkleTree::new();
        let commitment = crypto::hash_commitment(&withdrawal_hash, &nullifier);
        let index = tree.insert(commitment).unwrap();
        let merkle_proof = tree.prove(index).unwrap();

        let burn_proof = BurnProof::new(
            Hash::new(rng.random_bytes::<32>()),
            1000,
            [0u8; 20],
            vec![],
        );

        let prover = WithdrawalProver::new(4);
        let proof = prover
            .prove(
                &ProvingKey::from_bytes(vec![]),
                &withdrawal_hash,
                &nullifier,
                &merkle_proof,
                &burn_proof,
                1000,
            )
            .unwrap();

        let verifier = WithdrawalVerifier::new(4);

        // First verification should succeed
        let mut used_nullifiers = std::collections::HashSet::new();
        let result = verifier.verify_with_nullifier_check(
            &VerificationKey::from_bytes(vec![]),
            &proof,
            &used_nullifiers,
        );
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Add nullifier to used set
        used_nullifiers.insert(nullifier);

        // Second verification with same nullifier should fail
        let result = verifier.verify_with_nullifier_check(
            &VerificationKey::from_bytes(vec![]),
            &proof,
            &used_nullifiers,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_merkle_proof_rejected() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let nullifier = Hash::new(rng.random_bytes::<32>());

        // Create invalid Merkle proof
        let merkle_proof = merkle_tree::MerkleProof::new(
            Hash::new([1u8; 32]),
            0,
            vec![Hash::new([2u8; 32])],
            vec![merkle_tree::ProofPath::Left],
            Hash::new([99u8; 32]), // wrong root
        );

        let burn_proof = BurnProof::new(
            Hash::new(rng.random_bytes::<32>()),
            1000,
            [0u8; 20],
            vec![],
        );

        let prover = WithdrawalProver::new(4);
        let result = prover.prove(
            &ProvingKey::from_bytes(vec![]),
            &withdrawal_hash,
            &nullifier,
            &merkle_proof,
            &burn_proof,
            1000,
        );

        assert!(result.is_err());
    }
}

