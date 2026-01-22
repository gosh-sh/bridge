//! Incremental Merkle Tree implementation for the bridge
//!
//! This module implements a sparse Merkle tree similar to Tornado Cash's design.
//! The tree is optimized for efficient insertion and proof generation.

pub mod tree;
pub mod proof;
pub mod error;

pub use tree::{MerkleTree, TREE_HEIGHT};
pub use proof::{MerkleProof, ProofPath};
pub use error::{MerkleError, Result};

#[cfg(test)]
mod tests;

