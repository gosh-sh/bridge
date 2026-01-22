//! Integration tests for ZK proof system

use super::*;
use crypto::{hash_commitment, SecureRng};
use merkle_tree::MerkleTree;

#[test]
fn test_full_deposit_flow() {
    let mut rng = SecureRng::new();

    // Generate secrets
    let withdrawal_hash = crypto::Hash::new(rng.random_bytes::<32>());
    let nullifier = crypto::Hash::new(rng.random_bytes::<32>());
    let amount = 1000u64;

    // Create commitment
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);

    // Generate deposit proof
    let prover = deposit::DepositProver::new(4);
    let proof = prover
        .prove(&ProvingKey::from_bytes(vec![]), &withdrawal_hash, amount)
        .unwrap();

    // Verify deposit proof
    let verifier = deposit::DepositVerifier::new(4);
    assert!(verifier
        .verify(&VerificationKey::from_bytes(vec![]), &proof)
        .unwrap());
}

#[test]
fn test_full_withdrawal_flow() {
    let mut rng = SecureRng::new();

    // Generate secrets
    let withdrawal_hash = crypto::Hash::new(rng.random_bytes::<32>());
    let nullifier = crypto::Hash::new(rng.random_bytes::<32>());
    let amount = 1000u64;

    // Create commitment and add to Merkle tree
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    let mut tree = MerkleTree::new();
    let index = tree.insert(commitment).unwrap();
    let merkle_proof = tree.prove(index).unwrap();

    // Create burn proof
    let burn_proof = burn_proof::BurnProof::new(
        crypto::Hash::new(rng.random_bytes::<32>()),
        amount,
        [0u8; 20],
        vec![],
    );

    // Generate withdrawal proof
    let prover = withdrawal::WithdrawalProver::new(4);
    let proof = prover
        .prove(
            &ProvingKey::from_bytes(vec![]),
            &withdrawal_hash,
            &nullifier,
            &merkle_proof,
            &burn_proof,
            amount,
        )
        .unwrap();

    // Verify withdrawal proof
    let verifier = withdrawal::WithdrawalVerifier::new(4);
    assert!(verifier
        .verify(&VerificationKey::from_bytes(vec![]), &proof)
        .unwrap());
}

#[test]
fn test_end_to_end_bridge_flow() {
    let mut rng = SecureRng::new();

    // 1. User generates secrets
    let withdrawal_hash = crypto::Hash::new(rng.random_bytes::<32>());
    let nullifier = crypto::Hash::new(rng.random_bytes::<32>());
    let amount = 1000u64;

    // 2. Create commitment
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);

    // 3. Deposit: Add commitment to Merkle tree
    let mut tree = MerkleTree::new();
    let index = tree.insert(commitment).unwrap();

    // 4. Generate deposit proof
    let deposit_prover = deposit::DepositProver::new(4);
    let deposit_proof = deposit_prover
        .prove(&ProvingKey::from_bytes(vec![]), &withdrawal_hash, amount)
        .unwrap();

    // 5. Verify deposit proof
    let deposit_verifier = deposit::DepositVerifier::new(4);
    assert!(deposit_verifier
        .verify(&VerificationKey::from_bytes(vec![]), &deposit_proof)
        .unwrap());

    // 6. Later: User wants to withdraw
    let merkle_proof = tree.prove(index).unwrap();

    // 7. Create burn proof (from Acki Nacki)
    let burn_proof = burn_proof::BurnProof::new(
        crypto::Hash::new(rng.random_bytes::<32>()),
        amount,
        [1u8; 20],
        vec![],
    );

    // 8. Generate withdrawal proof
    let withdrawal_prover = withdrawal::WithdrawalProver::new(4);
    let withdrawal_proof = withdrawal_prover
        .prove(
            &ProvingKey::from_bytes(vec![]),
            &withdrawal_hash,
            &nullifier,
            &merkle_proof,
            &burn_proof,
            amount,
        )
        .unwrap();

    // 9. Verify withdrawal proof with nullifier check
    let withdrawal_verifier = withdrawal::WithdrawalVerifier::new(4);
    let used_nullifiers = std::collections::HashSet::new();
    assert!(withdrawal_verifier
        .verify_with_nullifier_check(
            &VerificationKey::from_bytes(vec![]),
            &withdrawal_proof,
            &used_nullifiers
        )
        .unwrap());
}

