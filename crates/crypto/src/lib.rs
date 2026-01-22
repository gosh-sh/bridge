//! Core cryptographic primitives for the Acki Nacki Bridge
//!
//! This crate provides ZK-friendly cryptographic operations including:
//! - Poseidon hash function (ZK-friendly)
//! - Random number generation (cryptographically secure)
//! - Field element operations (BN254 curve)
//!
//! # Examples
//!
//! ```
//! use crypto::{SecureRng, Hash, poseidon::hash_commitment};
//!
//! // Generate random values
//! let mut rng = SecureRng::new();
//! let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
//! let nullifier = Hash::new(rng.random_bytes::<32>());
//!
//! // Create commitment
//! let commitment = hash_commitment(&withdrawal_hash, &nullifier);
//! ```

pub mod poseidon;
pub mod random;
pub mod types;

pub use poseidon::{
    hash_commitment, hash_commitment_with_amount, poseidon_hash, poseidon_hash_two,
    PoseidonHasher,
};
pub use random::SecureRng;
pub use types::{FieldElement, Hash};

#[cfg(test)]
mod tests;

