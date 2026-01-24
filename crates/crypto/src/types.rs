//! Core cryptographic types for the bridge

use halo2_proofs::halo2curves::bn256::Fr;
use ff::{FromUniformBytes, PrimeField};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Field element used throughout the system (BN254 scalar field)
/// Using scroll-tech/halo2 which implements ff::PrimeField (required for Poseidon)
pub type FieldElement = Fr;

/// Hash output (32 bytes)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Create a new hash from bytes
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Create from field element
    pub fn from_field_element(fe: &FieldElement) -> Self {
        let bytes = fe.to_repr();
        Self(bytes.as_ref().try_into().expect("Field element should be 32 bytes"))
    }

    /// Convert to field element
    /// If the bytes don't represent a valid field element, they are reduced modulo the field order
    pub fn to_field_element(&self) -> FieldElement {
        // Use from_uniform_bytes which always succeeds by reducing mod field order
        // Pad to 64 bytes for uniform reduction
        let mut wide_bytes = [0u8; 64];
        wide_bytes[..32].copy_from_slice(&self.0);
        FieldElement::from_uniform_bytes(&wide_bytes)
    }

    /// Get as bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let bytes = hex::decode(s)?;
        if bytes.len() != 32 {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }

    /// Zero hash
    pub fn zero() -> Self {
        Self([0u8; 32])
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash(0x{})", self.to_hex())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Hash> for [u8; 32] {
    fn from(hash: Hash) -> Self {
        hash.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_creation() {
        let bytes = [1u8; 32];
        let hash = Hash::new(bytes);
        assert_eq!(hash.as_bytes(), &bytes);
    }

    #[test]
    fn test_hash_hex_conversion() {
        let hash = Hash::new([1u8; 32]);
        let hex = hash.to_hex();
        let decoded = Hash::from_hex(&hex).unwrap();
        assert_eq!(hash, decoded);
    }

    #[test]
    fn test_hash_field_element_conversion() {
        let fe = FieldElement::from(12345u64);
        let hash = Hash::from_field_element(&fe);
        let fe2 = hash.to_field_element();
        assert_eq!(fe, fe2);
    }

    #[test]
    fn test_zero_hash() {
        let zero = Hash::zero();
        assert_eq!(zero.as_bytes(), &[0u8; 32]);
    }
}

