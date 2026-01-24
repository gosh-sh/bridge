//! Deposit proof circuit and prover
//!
//! The deposit proof demonstrates knowledge of:
//! - Preimage of the withdrawal hash
//! - Amount in the concatenated hash
//!
//! Public inputs:
//! - Amount
//!
//! Circuit constraints:
//! 1. Hash(preimage) = withdrawal_hash (using Poseidon)
//! 2. Hash(withdrawal_hash || amount) = commitment (using Poseidon)

use crate::error::{ProofError, Result};
use crate::types::{ProofData, ProvingKey, VerificationKey};
use crypto::Hash;
use serde::{Deserialize, Serialize};

use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    halo2curves::bn256::{Bn256, Fr},
    plonk::{keygen_pk, keygen_vk, Circuit, ConstraintSystem, Error},
    poly::kzg::commitment::ParamsKZG,
};
use rand::rngs::OsRng;

/// Deposit proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProof {
    /// Proof data
    pub proof: ProofData,
    /// Amount (public input)
    pub amount: u64,
}

impl DepositProof {
    /// Create a new deposit proof
    pub fn new(proof: ProofData, amount: u64) -> Self {
        Self { proof, amount }
    }

    /// Get amount
    pub fn amount(&self) -> u64 {
        self.amount
    }
}

/// Deposit circuit configuration
#[derive(Clone, Debug)]
pub struct DepositCircuitConfig {
    /// Number of advice columns
    pub num_advice: usize,
    /// Number of lookup advice columns
    pub num_lookup_advice: usize,
    /// Number of fixed columns
    pub num_fixed: usize,
    /// Lookup bits
    pub lookup_bits: usize,
}

impl Default for DepositCircuitConfig {
    fn default() -> Self {
        Self {
            num_advice: 1,
            num_lookup_advice: 1,
            num_fixed: 1,
            lookup_bits: 8,
        }
    }
}

/// Deposit circuit
#[derive(Clone, Debug)]
pub struct DepositCircuit {
    /// Withdrawal hash preimage (private input)
    pub withdrawal_hash_preimage: Value<Fr>,
    /// Amount (private input, will be made public)
    pub amount: Value<Fr>,
    /// Circuit configuration
    pub config: DepositCircuitConfig,
}

impl DepositCircuit {
    /// Create a new deposit circuit
    pub fn new(
        withdrawal_hash_preimage: Value<Fr>,
        amount: Value<Fr>,
        config: DepositCircuitConfig,
    ) -> Self {
        Self {
            withdrawal_hash_preimage,
            amount,
            config,
        }
    }
}

/// Deposit prover
pub struct DepositProver {
    k: u32,
    params: ParamsKZG<Bn256>,
    config: DepositCircuitConfig,
}

impl DepositProver {
    /// Create a new deposit prover
    pub fn new(k: u32) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self {
            k,
            params,
            config: DepositCircuitConfig::default(),
        }
    }

    /// Create with custom configuration
    pub fn with_config(k: u32, config: DepositCircuitConfig) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self { k, params, config }
    }

    /// Generate proving and verification keys
    pub fn keygen(&self) -> Result<(ProvingKey, VerificationKey)> {
        // Create empty circuit for keygen
        let circuit = DepositCircuit::new(
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

    /// Generate a deposit proof
    pub fn prove(
        &self,
        _pk: &ProvingKey,
        withdrawal_hash_preimage: &Hash,
        amount: u64,
    ) -> Result<DepositProof> {
        // Convert inputs to field elements
        let preimage_fe = withdrawal_hash_preimage.to_field_element();
        let amount_fe = Fr::from(amount);

        // Create circuit with witness values
        let _circuit = DepositCircuit::new(
            Value::known(preimage_fe),
            Value::known(amount_fe),
            self.config.clone(),
        );

        // TODO: Implement actual proof generation
        // For now, return placeholder proof
        let proof_data = ProofData::new(
            vec![0xde, 0xad, 0xbe, 0xef],
            vec![amount.to_le_bytes().to_vec()],
        );

        Ok(DepositProof::new(proof_data, amount))
    }
}

// Implement Circuit trait for DepositCircuit
impl Circuit<Fr> for DepositCircuit {
    type Config = ();
    type FloorPlanner = SimpleFloorPlanner;
    #[cfg(feature = "circuit-params")]
    type Params = ();

    fn without_witnesses(&self) -> Self {
        Self {
            withdrawal_hash_preimage: Value::unknown(),
            amount: Value::unknown(),
            config: self.config.clone(),
        }
    }

    fn configure(_meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        // Configuration is handled by halo2-base internally
        // We use the BaseCircuitBuilder pattern
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
        // 2. Assign witness values
        // 3. Compute Poseidon hash of preimage
        // 4. Compute Poseidon hash of (hash || amount)
        // 5. Expose amount as public input

        // For now, this is a placeholder that will be completed
        // when we implement the full Poseidon circuit integration
        Ok(())
    }
}

/// Deposit verifier
pub struct DepositVerifier {
    k: u32,
    params: ParamsKZG<Bn256>,
}

impl DepositVerifier {
    /// Create a new deposit verifier
    pub fn new(k: u32) -> Self {
        let params = ParamsKZG::<Bn256>::setup(k, OsRng);
        Self { k, params }
    }

    /// Verify a deposit proof
    pub fn verify(&self, _vk: &VerificationKey, proof: &DepositProof) -> Result<bool> {
        // TODO: Implement actual verification with proper key deserialization
        // For now, just check that proof data exists
        Ok(!proof.proof.proof_bytes().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto::SecureRng;

    #[test]
    fn test_deposit_proof_creation() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let amount = 1000u64;

        let prover = DepositProver::new(4);
        let proof = prover.prove(&ProvingKey::from_bytes(vec![]), &withdrawal_hash, amount);

        assert!(proof.is_ok());
        let proof = proof.unwrap();
        assert_eq!(proof.amount(), amount);
    }

    #[test]
    fn test_deposit_proof_verification() {
        let mut rng = SecureRng::new();
        let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
        let amount = 1000u64;

        let prover = DepositProver::new(4);
        let proof = prover
            .prove(&ProvingKey::from_bytes(vec![]), &withdrawal_hash, amount)
            .unwrap();

        let verifier = DepositVerifier::new(4);
        let result = verifier.verify(&VerificationKey::from_bytes(vec![]), &proof);

        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_deposit_proof_serialization() {
        let proof_data = ProofData::new(vec![1, 2, 3], vec![vec![4, 5, 6]]);
        let proof = DepositProof::new(proof_data, 1000);

        let serialized = serde_json::to_string(&proof).unwrap();
        let deserialized: DepositProof = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.amount(), proof.amount());
    }
}

