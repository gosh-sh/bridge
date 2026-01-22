//! Integration tests for crypto module

use super::*;

#[test]
fn test_full_commitment_flow() {
    // Simulate the deposit flow
    let mut rng = SecureRng::new();

    // Generate withdrawal hash and nullifier
    let withdrawal_hash = Hash::new(rng.random_bytes::<32>());
    let nullifier = Hash::new(rng.random_bytes::<32>());

    // Create commitment
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);

    // Hash with amount
    let amount = 1000u64;
    let commitment_with_amount = hash_commitment_with_amount(&commitment, amount);

    // Verify determinism
    let commitment2 = hash_commitment(&withdrawal_hash, &nullifier);
    assert_eq!(commitment, commitment2);

    let commitment_with_amount2 = hash_commitment_with_amount(&commitment, amount);
    assert_eq!(commitment_with_amount, commitment_with_amount2);
}

#[test]
fn test_different_commitments() {
    let mut rng = SecureRng::new();

    let wh1 = Hash::new(rng.random_bytes::<32>());
    let n1 = Hash::new(rng.random_bytes::<32>());
    let commitment1 = hash_commitment(&wh1, &n1);

    let wh2 = Hash::new(rng.random_bytes::<32>());
    let n2 = Hash::new(rng.random_bytes::<32>());
    let commitment2 = hash_commitment(&wh2, &n2);

    // Different inputs should produce different commitments
    assert_ne!(commitment1, commitment2);
}

#[test]
fn test_hash_serialization() {
    let hash = Hash::new([42u8; 32]);
    let hex = hash.to_hex();
    let decoded = Hash::from_hex(&hex).unwrap();
    assert_eq!(hash, decoded);
}

