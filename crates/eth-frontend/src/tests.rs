//! Tests for Ethereum frontend

use crate::DepositManager;
use crypto::{hash_commitment, SecureRng};

#[test]
fn test_placeholder() {
    assert!(true);
}

#[test]
fn test_deposit_manager_creation() {
    let manager = DepositManager::new();
    assert!(std::mem::size_of_val(&manager) > 0);
}

#[test]
fn test_deposit_manager_default() {
    let manager = DepositManager::default();
    assert!(std::mem::size_of_val(&manager) > 0);
}

#[test]
fn test_generate_commitment() {
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();

    let commitment = hash_commitment(&withdrawal_hash, &nullifier);

    // Commitment should be deterministic
    let commitment2 = hash_commitment(&withdrawal_hash, &nullifier);
    assert_eq!(commitment, commitment2);
}

#[test]
fn test_different_commitments() {
    let mut rng = SecureRng::new();

    let withdrawal_hash1 = rng.random_hash();
    let nullifier1 = rng.random_hash();
    let commitment1 = hash_commitment(&withdrawal_hash1, &nullifier1);

    let withdrawal_hash2 = rng.random_hash();
    let nullifier2 = rng.random_hash();
    let commitment2 = hash_commitment(&withdrawal_hash2, &nullifier2);

    // Different inputs should produce different commitments
    assert_ne!(commitment1, commitment2);
}

#[test]
fn test_commitment_changes_with_withdrawal_hash() {
    let mut rng = SecureRng::new();
    let nullifier = rng.random_hash();

    let withdrawal_hash1 = rng.random_hash();
    let commitment1 = hash_commitment(&withdrawal_hash1, &nullifier);

    let withdrawal_hash2 = rng.random_hash();
    let commitment2 = hash_commitment(&withdrawal_hash2, &nullifier);

    assert_ne!(commitment1, commitment2);
}

#[test]
fn test_commitment_changes_with_nullifier() {
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();

    let nullifier1 = rng.random_hash();
    let commitment1 = hash_commitment(&withdrawal_hash, &nullifier1);

    let nullifier2 = rng.random_hash();
    let commitment2 = hash_commitment(&withdrawal_hash, &nullifier2);

    assert_ne!(commitment1, commitment2);
}

#[test]
fn test_random_hash_uniqueness() {
    let mut rng = SecureRng::new();

    let hash1 = rng.random_hash();
    let hash2 = rng.random_hash();
    let hash3 = rng.random_hash();

    // All should be different (with overwhelming probability)
    assert_ne!(hash1, hash2);
    assert_ne!(hash2, hash3);
    assert_ne!(hash1, hash3);
}

#[test]
fn test_deterministic_rng() {
    let seed = [42u8; 32];
    let mut rng1 = SecureRng::from_seed(seed);
    let mut rng2 = SecureRng::from_seed(seed);

    let hash1 = rng1.random_hash();
    let hash2 = rng2.random_hash();

    // Same seed should produce same hash
    assert_eq!(hash1, hash2);
}

