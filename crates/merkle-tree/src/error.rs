//! Error types for Merkle tree operations

use thiserror::Error;

/// Result type for Merkle tree operations
pub type Result<T> = std::result::Result<T, MerkleError>;

/// Errors that can occur during Merkle tree operations
#[derive(Error, Debug, Clone, PartialEq)]
pub enum MerkleError {
    /// Tree is full (maximum capacity reached)
    #[error("Merkle tree is full (capacity: {0})")]
    TreeFull(usize),

    /// Invalid leaf index
    #[error("Invalid leaf index: {index} (max: {max})")]
    InvalidIndex { index: usize, max: usize },

    /// Invalid proof
    #[error("Invalid Merkle proof")]
    InvalidProof,

    /// Invalid tree height
    #[error("Invalid tree height: {0}")]
    InvalidHeight(usize),

    /// Leaf not found
    #[error("Leaf not found at index {0}")]
    LeafNotFound(usize),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),
}

