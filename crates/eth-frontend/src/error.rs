//! Error types for the Ethereum frontend

use thiserror::Error;

/// Result type for bridge operations
pub type Result<T> = std::result::Result<T, BridgeError>;

/// Errors that can occur in the bridge
#[derive(Error, Debug)]
pub enum BridgeError {
    /// Ethereum contract error
    #[error("Ethereum contract error: {0}")]
    ContractError(String),

    /// Acki Nacki error
    #[error("Acki Nacki error: {0}")]
    AckiNackiError(String),

    /// Proof error
    #[error("Proof error: {0}")]
    ProofError(String),

    /// Merkle tree error
    #[error("Merkle tree error: {0}")]
    MerkleTreeError(String),

    /// Invalid state
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Transaction failed
    #[error("Transaction failed: {0}")]
    TransactionFailed(String),

    /// Timeout
    #[error("Operation timed out")]
    Timeout,

    /// Not found
    #[error("Not found: {0}")]
    NotFound(String),
}
