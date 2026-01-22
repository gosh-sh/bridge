//! Poseidon hash function implementation
//!
//! Poseidon is a ZK-friendly hash function designed for use in zero-knowledge proofs.
//! It's particularly efficient in arithmetic circuits.
//!
//! This implementation uses poseidon-primitives (same library used by halo2-base)
//! for native Poseidon operations. The circuit implementation will use halo2-base's
//! Poseidon chip.

use crate::types::{FieldElement, Hash};
use pse_poseidon::Poseidon;

/// Poseidon hasher for BN254 curve
///
/// Uses poseidon-primitives for native (non-circuit) Poseidon hashing.
/// This is the same library used internally by halo2-base.
///
/// Uses P128Pow5T3 spec (Poseidon-128 with x^5 S-box, width 3, rate 2)
pub struct PoseidonHasher {
    _phantom: std::marker::PhantomData<FieldElement>,
}

impl PoseidonHasher {
    /// Create a new Poseidon hasher
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }

    /// Hash a single field element using Poseidon
    pub fn hash_one(&mut self, input: &FieldElement) -> FieldElement {
        // pse-poseidon uses generic PrimeField, which ark_bn254::Fr implements
        let mut poseidon = Poseidon::<FieldElement, 3, 2>::new(8, 57);
        poseidon.update(&[*input]);
        poseidon.squeeze()
    }

    /// Hash two field elements using Poseidon
    pub fn hash_two(&mut self, left: &FieldElement, right: &FieldElement) -> FieldElement {
        // pse-poseidon uses generic PrimeField, which ark_bn254::Fr implements
        let mut poseidon = Poseidon::<FieldElement, 3, 2>::new(8, 57);
        poseidon.update(&[*left, *right]);
        poseidon.squeeze()
    }

    /// Hash multiple field elements using Poseidon
    ///
    /// Note: For variable-length inputs, we hash them iteratively in pairs
    /// This ensures compatibility with the circuit implementation
    pub fn hash_many(&mut self, inputs: &[FieldElement]) -> FieldElement {
        if inputs.is_empty() {
            return FieldElement::from(0u64);
        }

        if inputs.len() == 1 {
            return self.hash_one(&inputs[0]);
        }

        // Hash iteratively: hash(hash(a, b), c), etc.
        let mut result = self.hash_two(&inputs[0], &inputs[1]);
        for input in &inputs[2..] {
            result = self.hash_two(&result, input);
        }
        result
    }

    /// Hash bytes by converting to field elements
    pub fn hash_bytes(&mut self, bytes: &[u8]) -> Hash {
        // Convert bytes to field elements (chunking if necessary)
        use ff::PrimeField;
        let mut field_elements = Vec::new();
        for chunk in bytes.chunks(31) {
            // Use 31 bytes to ensure we stay within field size
            let mut bytes_32 = [0u8; 32];
            bytes_32[..chunk.len()].copy_from_slice(chunk);
            let mut repr = <FieldElement as PrimeField>::Repr::default();
            repr.as_mut().copy_from_slice(&bytes_32);
            let fe = FieldElement::from_repr(repr).unwrap_or(FieldElement::zero());
            field_elements.push(fe);
        }

        let result = self.hash_many(&field_elements);
        Hash::from_field_element(&result)
    }
}

impl Default for PoseidonHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function to hash a single field element
pub fn poseidon_hash(input: &FieldElement) -> FieldElement {
    let mut hasher = PoseidonHasher::new();
    hasher.hash_one(input)
}

/// Convenience function to hash two field elements
pub fn poseidon_hash_two(left: &FieldElement, right: &FieldElement) -> FieldElement {
    let mut hasher = PoseidonHasher::new();
    hasher.hash_two(left, right)
}

/// Hash a withdrawal hash and nullifier together
pub fn hash_commitment(withdrawal_hash: &Hash, nullifier: &Hash) -> Hash {
    let mut hasher = PoseidonHasher::new();
    let wh_fe = withdrawal_hash.to_field_element();
    let n_fe = nullifier.to_field_element();
    let result = hasher.hash_two(&wh_fe, &n_fe);
    Hash::from_field_element(&result)
}

/// Hash a commitment with an amount
pub fn hash_commitment_with_amount(commitment: &Hash, amount: u64) -> Hash {
    let mut hasher = PoseidonHasher::new();
    let commitment_fe = commitment.to_field_element();
    let amount_fe = FieldElement::from(amount);
    let result = hasher.hash_two(&commitment_fe, &amount_fe);
    Hash::from_field_element(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poseidon_hash_deterministic() {
        let input = FieldElement::from(12345u64);
        let hash1 = poseidon_hash(&input);
        let hash2 = poseidon_hash(&input);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_poseidon_hash_two() {
        let left = FieldElement::from(111u64);
        let right = FieldElement::from(222u64);
        let hash1 = poseidon_hash_two(&left, &right);
        let hash2 = poseidon_hash_two(&left, &right);
        assert_eq!(hash1, hash2);

        // Different order should give different hash
        let hash3 = poseidon_hash_two(&right, &left);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_hash_commitment() {
        let wh = Hash::new([1u8; 32]);
        let nullifier = Hash::new([2u8; 32]);

        let commitment1 = hash_commitment(&wh, &nullifier);
        let commitment2 = hash_commitment(&wh, &nullifier);
        assert_eq!(commitment1, commitment2);

        // Different inputs should give different hash
        let wh2 = Hash::new([3u8; 32]);
        let commitment3 = hash_commitment(&wh2, &nullifier);
        assert_ne!(commitment1, commitment3);
    }

    #[test]
    fn test_hash_commitment_with_amount() {
        let commitment = Hash::new([1u8; 32]);
        let amount = 1000u64;

        let hash1 = hash_commitment_with_amount(&commitment, amount);
        let hash2 = hash_commitment_with_amount(&commitment, amount);
        assert_eq!(hash1, hash2);

        // Different amount should give different hash
        let hash3 = hash_commitment_with_amount(&commitment, 2000);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_hash_bytes() {
        let mut hasher = PoseidonHasher::new();
        let bytes = b"Hello, Acki Nacki Bridge!";
        let hash1 = hasher.hash_bytes(bytes);

        let mut hasher2 = PoseidonHasher::new();
        let hash2 = hasher2.hash_bytes(bytes);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_collision_resistance() {
        // Test that different inputs produce different outputs
        let input1 = FieldElement::from(1u64);
        let input2 = FieldElement::from(2u64);

        let hash1 = poseidon_hash(&input1);
        let hash2 = poseidon_hash(&input2);

        assert_ne!(hash1, hash2);
    }
}

