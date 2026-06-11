//! Error types for Acki Nacki operations

use thiserror::Error;

/// Result type for Acki Nacki operations
pub type Result<T> = std::result::Result<T, AckiNackiError>;

/// Errors that can occur during Acki Nacki operations.
///
/// `Clone` is intentionally NOT derived: HTTP/JSON variants store the
/// underlying error type by string for portability, while still surfacing the
/// original `Display` rendering.
#[derive(Error, Debug)]
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

    /// HTTP request to the Acki Nacki node failed (connection / DNS / TLS /
    /// non-2xx status / timeout).
    #[error("HTTP request to AN node failed: {0}")]
    Http(String),

    /// JSON body returned by the node could not be parsed against the expected
    /// schema. Almost always a node-side schema drift; bring it up with the AN
    /// team before patching the client.
    #[error("JSON decode error: {0}")]
    JsonParse(String),

    /// Returned bytes had wrong length (e.g. BLS pubkey not exactly 48 bytes).
    #[error("invalid bytes length in {field}: expected {expected}, got {actual}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },

    /// Address string is not in the SDK 3.0 `dapp_id::account_id` form.
    #[error("invalid address: {0}")]
    InvalidAddress(String),
}

impl From<reqwest::Error> for AckiNackiError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.to_string())
    }
}

impl From<serde_json::Error> for AckiNackiError {
    fn from(e: serde_json::Error) -> Self {
        Self::JsonParse(e.to_string())
    }
}
