//! Error types for Acki Nacki operations

use thiserror::Error;

/// Result type for Acki Nacki operations
pub type Result<T> = std::result::Result<T, AckiNackiError>;

/// Errors that can occur during Acki Nacki operations
#[derive(Error, Debug, Clone, PartialEq)]
pub enum AckiNackiError {
    /// Transaction failed
    #[error("Transaction failed: {0}")]
    TransactionFailed(String),

    /// Transaction not found
    #[error("Transaction not found: {0}")]
    TransactionNotFound(String),

    /// Network error
    #[error("Network error: {0}")]
    NetworkError(String),

    /// Invalid transaction
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),

    /// Insufficient balance
    #[error("Insufficient balance")]
    InsufficientBalance,

    /// Contract error
    #[error("Contract error: {0}")]
    ContractError(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Timeout
    #[error("Operation timed out")]
    Timeout,

    /// Not implemented
    #[error("Not implemented (placeholder for Acki Nacki team)")]
    NotImplemented,
}

