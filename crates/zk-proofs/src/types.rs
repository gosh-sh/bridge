//! Common types for ZK proofs

use serde::{Deserialize, Serialize};

/// Proof data (serialized Halo2 proof)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofData {
    /// Serialized proof bytes
    pub proof: Vec<u8>,
    /// Public inputs
    pub public_inputs: Vec<Vec<u8>>,
}

impl ProofData {
    /// Create new proof data
    pub fn new(proof: Vec<u8>, public_inputs: Vec<Vec<u8>>) -> Self {
        Self {
            proof,
            public_inputs,
        }
    }

    /// Get proof bytes
    pub fn proof_bytes(&self) -> &[u8] {
        &self.proof
    }

    /// Get public inputs
    pub fn public_inputs(&self) -> &[Vec<u8>] {
        &self.public_inputs
    }
}

/// Verification key for a circuit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationKey {
    /// Serialized verification key
    pub vk_bytes: Vec<u8>,
}

impl VerificationKey {
    /// Create from bytes
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { vk_bytes: bytes }
    }

    /// Get bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.vk_bytes
    }
}

/// Proving key for a circuit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvingKey {
    /// Serialized proving key
    pub pk_bytes: Vec<u8>,
}

impl ProvingKey {
    /// Create from bytes
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { pk_bytes: bytes }
    }

    /// Get bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.pk_bytes
    }
}

