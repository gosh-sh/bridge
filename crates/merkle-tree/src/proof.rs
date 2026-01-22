//! Merkle proof types and verification

use crypto::Hash;
use serde::{Deserialize, Serialize};

/// Direction in the Merkle tree (left or right sibling)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofPath {
    /// Current node is left child, sibling is on the right
    Left,
    /// Current node is right child, sibling is on the left
    Right,
}

/// Merkle proof for inclusion of a leaf in the tree
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    /// The leaf being proven
    pub leaf: Hash,
    /// Index of the leaf in the tree
    pub leaf_index: usize,
    /// Sibling hashes along the path from leaf to root
    pub siblings: Vec<Hash>,
    /// Path directions (left or right) for each level
    pub path: Vec<ProofPath>,
    /// Root hash of the tree
    pub root: Hash,
}

impl MerkleProof {
    /// Create a new Merkle proof
    pub fn new(
        leaf: Hash,
        leaf_index: usize,
        siblings: Vec<Hash>,
        path: Vec<ProofPath>,
        root: Hash,
    ) -> Self {
        Self {
            leaf,
            leaf_index,
            siblings,
            path,
            root,
        }
    }

    /// Verify the proof
    pub fn verify(&self) -> bool {
        if self.siblings.len() != self.path.len() {
            return false;
        }

        let mut current_hash = self.leaf;

        for (sibling, direction) in self.siblings.iter().zip(self.path.iter()) {
            current_hash = match direction {
                ProofPath::Left => {
                    // Current node is left, sibling is right
                    hash_pair(&current_hash, sibling)
                }
                ProofPath::Right => {
                    // Current node is right, sibling is left
                    hash_pair(sibling, &current_hash)
                }
            };
        }

        current_hash == self.root
    }

    /// Get the height of the tree (number of levels)
    pub fn height(&self) -> usize {
        self.siblings.len()
    }
}

/// Hash two nodes together (left, right)
fn hash_pair(left: &Hash, right: &Hash) -> Hash {
    use crypto::poseidon::poseidon_hash_two;
    let left_fe = left.to_field_element();
    let right_fe = right.to_field_element();
    let result = poseidon_hash_two(&left_fe, &right_fe);
    Hash::from_field_element(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proof_path() {
        let left = ProofPath::Left;
        let right = ProofPath::Right;
        assert_ne!(left, right);
    }

    #[test]
    fn test_hash_pair_deterministic() {
        let left = Hash::new([1u8; 32]);
        let right = Hash::new([2u8; 32]);

        let hash1 = hash_pair(&left, &right);
        let hash2 = hash_pair(&left, &right);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_pair_order_matters() {
        let left = Hash::new([1u8; 32]);
        let right = Hash::new([2u8; 32]);

        let hash1 = hash_pair(&left, &right);
        let hash2 = hash_pair(&right, &left);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_simple_proof_verification() {
        // Create a simple 2-level tree
        // Root
        //  / \
        // L0 L1
        let leaf0 = Hash::new([1u8; 32]);
        let leaf1 = Hash::new([2u8; 32]);
        let root = hash_pair(&leaf0, &leaf1);

        // Proof for leaf0
        let proof = MerkleProof::new(
            leaf0,
            0,
            vec![leaf1], // sibling
            vec![ProofPath::Left], // leaf0 is left child
            root,
        );

        assert!(proof.verify());
    }

    #[test]
    fn test_invalid_proof() {
        let leaf = Hash::new([1u8; 32]);
        let sibling = Hash::new([2u8; 32]);
        let wrong_root = Hash::new([99u8; 32]);

        let proof = MerkleProof::new(leaf, 0, vec![sibling], vec![ProofPath::Left], wrong_root);

        assert!(!proof.verify());
    }
}

