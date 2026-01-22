//! Secure random number generation

use rand::{CryptoRng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;

/// Secure random number generator
pub struct SecureRng {
    rng: ChaCha20Rng,
}

impl SecureRng {
    /// Create a new secure RNG from OS entropy
    pub fn new() -> Self {
        Self {
            rng: ChaCha20Rng::from_entropy(),
        }
    }

    /// Create from a seed (for testing)
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            rng: ChaCha20Rng::from_seed(seed),
        }
    }

    /// Generate random bytes
    pub fn random_bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut bytes = [0u8; N];
        self.rng.fill_bytes(&mut bytes);
        bytes
    }

    /// Generate a random field element
    pub fn random_field_element(&mut self) -> crate::types::FieldElement {
        use ff::Field;
        crate::types::FieldElement::random(&mut self.rng)
    }

    /// Generate a random hash
    pub fn random_hash(&mut self) -> crate::types::Hash {
        crate::types::Hash::new(self.random_bytes::<32>())
    }
}

impl Default for SecureRng {
    fn default() -> Self {
        Self::new()
    }
}

impl RngCore for SecureRng {
    fn next_u32(&mut self) -> u32 {
        self.rng.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.rng.fill_bytes(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.rng.try_fill_bytes(dest)
    }
}

impl CryptoRng for SecureRng {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_bytes() {
        let mut rng = SecureRng::new();
        let bytes1 = rng.random_bytes::<32>();
        let bytes2 = rng.random_bytes::<32>();
        // Should be different (with overwhelming probability)
        assert_ne!(bytes1, bytes2);
    }

    #[test]
    fn test_random_field_element() {
        let mut rng = SecureRng::new();
        let fe1 = rng.random_field_element();
        let fe2 = rng.random_field_element();
        assert_ne!(fe1, fe2);
    }

    #[test]
    fn test_deterministic_from_seed() {
        let seed = [42u8; 32];
        let mut rng1 = SecureRng::from_seed(seed);
        let mut rng2 = SecureRng::from_seed(seed);

        let bytes1 = rng1.random_bytes::<32>();
        let bytes2 = rng2.random_bytes::<32>();
        assert_eq!(bytes1, bytes2);
    }
}

