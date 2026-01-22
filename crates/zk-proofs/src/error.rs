//! Error types for ZK proof operations

use thiserror::Error;

/// Result type for proof operations
pub type Result<T> = std::result::Result<T, ProofError>;

/// Errors that can occur during proof operations
#[derive(Error, Debug, Clone, PartialEq)]
pub enum ProofError {
    /// Proof generation failed
    #[error("Proof generation failed: {0}")]
    GenerationFailed(String),

    /// Proof verification failed
    #[error("Proof verification failed: {0}")]
    VerificationFailed(String),

    /// Invalid witness data
    #[error("Invalid witness data: {0}")]
    InvalidWitness(String),

    /// Invalid public inputs
    #[error("Invalid public inputs: {0}")]
    InvalidPublicInputs(String),

    /// Circuit error
    #[error("Circuit error: {0}")]
    CircuitError(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Invalid proof
    #[error("Invalid proof")]
    InvalidProof,

    /// Missing proving key
    #[error("Missing proving key")]
    MissingProvingKey,

    /// Missing verification key
    #[error("Missing verification key")]
    MissingVerificationKey,
}

