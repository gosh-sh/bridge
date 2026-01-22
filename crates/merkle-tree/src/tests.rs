//! Comprehensive tests for Merkle tree
//!
//! These tests are organized into three categories:
//! 1. Definitely broken tests - tests that expose real bugs
//! 2. Unclear tests - tests that may have bugs in test or code
//! 3. Passing tests - tests that verify correct behavior

use super::*;
use crypto::{Hash, SecureRng};

// ============================================================================
// PASSING TESTS - Verify correct behavior
// ============================================================================

#[test]
fn test_sequential_insertions() {
    let mut tree = MerkleTree::with_height(10);
    let mut rng = SecureRng::from_seed([1u8; 32]);

    for i in 0..100 {
        let leaf = Hash::new(rng.random_bytes::<32>());
        let index = tree.insert(leaf).unwrap();
        assert_eq!(index, i);

        // Verify proof for each insertion
        let proof = tree.prove(index).unwrap();
        assert!(proof.verify());
    }
}

#[test]
fn test_root_uniqueness() {
    let mut tree1 = MerkleTree::new();
    let mut tree2 = MerkleTree::new();

    let leaf1 = Hash::new([1u8; 32]);
    let leaf2 = Hash::new([2u8; 32]);

    tree1.insert(leaf1).unwrap();
    tree2.insert(leaf2).unwrap();

    // Different leaves should produce different roots
    assert_ne!(tree1.root(), tree2.root());
}

#[test]
fn test_proof_path_correctness() {
    let mut tree = MerkleTree::with_height(4);

    // Insert leaves
    for i in 0..8 {
        let leaf = Hash::new([i as u8; 32]);
        tree.insert(leaf).unwrap();
    }

    // Check proof for leaf at index 5 (binary: 101)
    let proof = tree.prove(5).unwrap();
    assert_eq!(proof.leaf_index, 5);
    assert_eq!(proof.height(), 4);

    // Verify path directions
    // Index 5 = 0b101
    // Level 0: 1 (right)
    // Level 1: 0 (left)
    // Level 2: 1 (right)
    // Level 3: 0 (left)
    assert_eq!(proof.path[0], ProofPath::Right); // bit 0
    assert_eq!(proof.path[1], ProofPath::Left);  // bit 1
    assert_eq!(proof.path[2], ProofPath::Right); // bit 2
    assert_eq!(proof.path[3], ProofPath::Left);  // bit 3
}

#[test]
fn test_empty_tree_root() {
    let tree1 = MerkleTree::with_height(5);
    let tree2 = MerkleTree::with_height(5);

    // Empty trees with same height should have same root
    assert_eq!(tree1.root(), tree2.root());
}

#[test]
fn test_tree_serialization() {
    let mut tree = MerkleTree::new();

    for i in 0..10 {
        let leaf = Hash::new([i as u8; 32]);
        tree.insert(leaf).unwrap();
    }

    let root_before = tree.root();

    // Serialize and deserialize
    let serialized = serde_json::to_string(&tree).unwrap();
    let deserialized: MerkleTree = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.root(), root_before);
    assert_eq!(deserialized.len(), tree.len());
}

#[test]
fn test_proof_serialization() {
    let mut tree = MerkleTree::new();
    let leaf = Hash::new([42u8; 32]);
    let index = tree.insert(leaf).unwrap();

    let proof = tree.prove(index).unwrap();

    // Serialize and deserialize
    let serialized = serde_json::to_string(&proof).unwrap();
    let deserialized: MerkleProof = serde_json::from_str(&serialized).unwrap();

    assert!(deserialized.verify());
    assert_eq!(deserialized.leaf, proof.leaf);
    assert_eq!(deserialized.root, proof.root);
}

// ============================================================================
// SECURITY TESTS - Test attack resistance
// ============================================================================

#[test]
fn test_cannot_forge_proof() {
    let mut tree = MerkleTree::new();

    // Insert legitimate leaf
    let real_leaf = Hash::new([1u8; 32]);
    tree.insert(real_leaf).unwrap();

    // Try to create proof for non-existent leaf
    let fake_leaf = Hash::new([99u8; 32]);
    let real_proof = tree.prove(0).unwrap();

    // Create fake proof with wrong leaf
    let fake_proof = MerkleProof::new(
        fake_leaf,
        0,
        real_proof.siblings.clone(),
        real_proof.path.clone(),
        real_proof.root,
    );

    // Fake proof should not verify
    assert!(!fake_proof.verify());
}

#[test]
fn test_cannot_reuse_proof_for_different_root() {
    let mut tree = MerkleTree::new();

    let leaf1 = Hash::new([1u8; 32]);
    tree.insert(leaf1).unwrap();

    let proof1 = tree.prove(0).unwrap();
    let root1 = tree.root();

    // Insert another leaf (changes root)
    let leaf2 = Hash::new([2u8; 32]);
    tree.insert(leaf2).unwrap();

    let root2 = tree.root();
    assert_ne!(root1, root2);

    // Old proof should still verify against old root
    assert!(proof1.verify());

    // But tree's current root is different
    assert_ne!(proof1.root, tree.root());
}

#[test]
fn test_collision_resistance() {
    let mut tree = MerkleTree::new();
    let mut roots = std::collections::HashSet::new();

    // Insert many leaves and check for root collisions
    for i in 0..100 {
        let leaf = Hash::new([i as u8; 32]);
        tree.insert(leaf).unwrap();
        let root = tree.root();

        // Each root should be unique
        assert!(roots.insert(root), "Root collision detected at index {}", i);
    }
}

// ============================================================================
// EDGE CASE TESTS
// ============================================================================

#[test]
fn test_single_leaf_tree() {
    let mut tree = MerkleTree::with_height(1);
    let leaf = Hash::new([42u8; 32]);

    tree.insert(leaf).unwrap();
    let proof = tree.prove(0).unwrap();

    assert!(proof.verify());
    assert_eq!(proof.height(), 1);
}

#[test]
fn test_full_small_tree() {
    let height = 3;
    let mut tree = MerkleTree::with_height(height);
    let capacity = 1 << height;

    // Fill the tree
    for i in 0..capacity {
        let leaf = Hash::new([i as u8; 32]);
        tree.insert(leaf).unwrap();
    }

    assert_eq!(tree.len(), capacity);

    // Try to insert one more (should fail)
    let extra_leaf = Hash::new([99u8; 32]);
    let result = tree.insert(extra_leaf);
    assert!(result.is_err());
    assert!(matches!(result, Err(MerkleError::TreeFull(_))));
}

#[test]
fn test_get_leaf() {
    let mut tree = MerkleTree::new();
    let leaf = Hash::new([42u8; 32]);

    let index = tree.insert(leaf).unwrap();
    let retrieved = tree.get_leaf(index).unwrap();

    assert_eq!(*retrieved, leaf);
}

#[test]
fn test_contains() {
    let mut tree = MerkleTree::new();
    let leaf = Hash::new([42u8; 32]);

    assert!(!tree.contains(0));

    tree.insert(leaf).unwrap();

    assert!(tree.contains(0));
    assert!(!tree.contains(1));
}

// ============================================================================
// PROPERTY-BASED TESTS (using simple randomization)
// ============================================================================

#[test]
fn test_proof_verification_property() {
    let mut rng = SecureRng::from_seed([42u8; 32]);
    let mut tree = MerkleTree::with_height(10);

    // Insert random leaves
    for _ in 0..50 {
        let leaf = Hash::new(rng.random_bytes::<32>());
        tree.insert(leaf).unwrap();
    }

    // Property: All proofs should verify
    for i in 0..tree.len() {
        let proof = tree.prove(i).unwrap();
        assert!(
            proof.verify(),
            "Proof verification failed for index {}",
            i
        );
        assert!(tree.verify(&proof), "Tree verification failed for index {}", i);
    }
}

#[test]
fn test_root_determinism_property() {
    let mut rng = SecureRng::from_seed([123u8; 32]);

    // Create two trees with same insertions
    let mut tree1 = MerkleTree::new();
    let mut tree2 = MerkleTree::new();

    let leaves: Vec<Hash> = (0..20)
        .map(|_| Hash::new(rng.random_bytes::<32>()))
        .collect();

    for leaf in &leaves {
        tree1.insert(*leaf).unwrap();
        tree2.insert(*leaf).unwrap();
    }

    // Property: Same insertions should produce same root
    assert_eq!(tree1.root(), tree2.root());
}

// ============================================================================
// UNCLEAR TESTS - May indicate bugs in tests or code
// ============================================================================

// Note: These tests are currently passing but may need review

// ============================================================================
// DEFINITELY BROKEN TESTS - Currently no known bugs
// ============================================================================

// Note: If any tests fail, they should be categorized here

