//! Incremental Merkle Tree implementation
//!
//! This is a sparse Merkle tree optimized for sequential insertions.
//! It maintains a cache of intermediate hashes for efficient updates.

use crate::error::{MerkleError, Result};
use crate::proof::{MerkleProof, ProofPath};
use crypto::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Height of the Merkle tree (20 levels = 2^20 = ~1M leaves)
pub const TREE_HEIGHT: usize = 20;

/// Maximum number of leaves in the tree
pub const MAX_LEAVES: usize = 1 << TREE_HEIGHT;

/// Incremental Merkle Tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleTree {
    /// Height of the tree
    height: usize,
    /// Current number of leaves
    next_index: usize,
    /// Cache of zero hashes for each level
    zero_hashes: Vec<Hash>,
    /// Filled subtrees (rightmost node at each level)
    filled_subtrees: Vec<Hash>,
    /// Optional: store all leaves for proof generation
    leaves: HashMap<usize, Hash>,
}

impl MerkleTree {
    /// Create a new Merkle tree with default height
    pub fn new() -> Self {
        Self::with_height(TREE_HEIGHT)
    }

    /// Create a new Merkle tree with specified height
    pub fn with_height(height: usize) -> Self {
        let zero_hashes = Self::compute_zero_hashes(height);
        let filled_subtrees = zero_hashes.clone();

        Self {
            height,
            next_index: 0,
            zero_hashes,
            filled_subtrees,
            leaves: HashMap::new(),
        }
    }

    /// Compute zero hashes for each level
    /// zero_hashes[0] = hash(0)
    /// zero_hashes[i] = hash(zero_hashes[i-1], zero_hashes[i-1])
    fn compute_zero_hashes(height: usize) -> Vec<Hash> {
        let mut zero_hashes = Vec::with_capacity(height + 1);
        zero_hashes.push(Hash::zero());

        for i in 0..height {
            let prev = &zero_hashes[i];
            let next = hash_pair(prev, prev);
            zero_hashes.push(next);
        }

        zero_hashes
    }

    /// Insert a new leaf into the tree
    ///
    /// This implements the incremental Merkle tree algorithm from Tornado Cash.
    /// The filled_subtrees array stores the right-most hash at each level.
    pub fn insert(&mut self, leaf: Hash) -> Result<usize> {
        let max_leaves = 1 << self.height;
        if self.next_index >= max_leaves {
            return Err(MerkleError::TreeFull(max_leaves));
        }

        let index = self.next_index;
        self.leaves.insert(index, leaf);

        // Update the tree using the incremental algorithm
        let mut current_hash = leaf;
        let mut current_index = index;

        for level in 0..self.height {
            if current_index % 2 == 0 {
                // Left node - store it and wait for right sibling
                self.filled_subtrees[level] = current_hash;
                break;
            } else {
                // Right node - hash with left sibling from filled_subtrees
                let left_sibling = &self.filled_subtrees[level];
                current_hash = hash_pair(left_sibling, &current_hash);
                current_index /= 2;
            }
        }

        self.next_index += 1;
        Ok(index)
    }

    /// Get the current root of the tree
    ///
    /// This computes the root by starting from the last inserted leaf
    /// and building up the path to the root using filled_subtrees and zero_hashes.
    pub fn root(&self) -> Hash {
        if self.next_index == 0 {
            // Empty tree - return the zero hash at the root level
            return self.zero_hashes[self.height];
        }

        // Start from the last inserted leaf (get it from the leaves map)
        let last_leaf_index = self.next_index - 1;
        let mut current_hash = self.leaves.get(&last_leaf_index)
            .copied()
            .unwrap_or(self.zero_hashes[0]);
        let mut index = last_leaf_index;

        for level in 0..self.height {
            let is_right = index % 2 == 1;

            if is_right {
                // We are right child, left sibling is in filled_subtrees
                let left_sibling = &self.filled_subtrees[level];
                current_hash = hash_pair(left_sibling, &current_hash);
            } else {
                // We are left child, right sibling is zero hash
                let right_zero = &self.zero_hashes[level];
                current_hash = hash_pair(&current_hash, right_zero);
            }

            index /= 2;
        }

        current_hash
    }

    /// Generate a Merkle proof for a leaf at the given index
    ///
    /// This implements proof generation for an incremental Merkle tree.
    /// For each level, we determine the sibling based on whether we're a left or right child.
    pub fn prove(&self, leaf_index: usize) -> Result<MerkleProof> {
        if self.next_index == 0 {
            return Err(MerkleError::InvalidIndex {
                index: leaf_index,
                max: 0,
            });
        }

        if leaf_index >= self.next_index {
            return Err(MerkleError::InvalidIndex {
                index: leaf_index,
                max: self.next_index - 1,
            });
        }

        let leaf = *self
            .leaves
            .get(&leaf_index)
            .ok_or(MerkleError::LeafNotFound(leaf_index))?;

        let mut siblings = Vec::new();
        let mut path = Vec::new();
        let mut index = leaf_index;
        let mut cache = HashMap::new();

        // For each level, compute the sibling
        for level in 0..self.height {
            let is_right = index % 2 == 1;

            let sibling_index = if is_right { index - 1 } else { index + 1 };
            let sibling_hash = self.compute_subtree_hash_cached(sibling_index, level, &mut cache);

            siblings.push(sibling_hash);
            path.push(if is_right {
                ProofPath::Right
            } else {
                ProofPath::Left
            });

            index /= 2;
        }

        Ok(MerkleProof::new(
            leaf,
            leaf_index,
            siblings,
            path,
            self.root(),
        ))
    }

    /// Compute the hash of a subtree rooted at (level, index) with caching
    fn compute_subtree_hash_cached(
        &self,
        node_index: usize,
        level: usize,
        cache: &mut HashMap<(usize, usize), Hash>,
    ) -> Hash {
        // Check cache first
        if let Some(&hash) = cache.get(&(level, node_index)) {
            return hash;
        }

        // Base case: if this subtree is beyond our leaves, return zero hash
        let leaf_start = node_index << level;
        if leaf_start >= self.next_index {
            return self.zero_hashes[level];
        }

        // Base case: if we're at leaf level
        if level == 0 {
            return self.leaves.get(&node_index)
                .copied()
                .unwrap_or(self.zero_hashes[0]);
        }

        // Recursive case: compute left and right children
        let left_child = node_index * 2;
        let right_child = node_index * 2 + 1;

        let left_hash = self.compute_subtree_hash_cached(left_child, level - 1, cache);
        let right_hash = self.compute_subtree_hash_cached(right_child, level - 1, cache);

        let hash = hash_pair(&left_hash, &right_hash);
        cache.insert((level, node_index), hash);
        hash
    }

    /// Verify a Merkle proof
    pub fn verify(&self, proof: &MerkleProof) -> bool {
        proof.verify() && proof.root == self.root()
    }

    /// Get the number of leaves in the tree
    pub fn len(&self) -> usize {
        self.next_index
    }

    /// Check if the tree is empty
    pub fn is_empty(&self) -> bool {
        self.next_index == 0
    }

    /// Get the height of the tree
    pub fn height(&self) -> usize {
        self.height
    }

    /// Get the maximum capacity of the tree
    pub fn capacity(&self) -> usize {
        1 << self.height
    }

    /// Check if a leaf exists at the given index
    pub fn contains(&self, index: usize) -> bool {
        index < self.next_index && self.leaves.contains_key(&index)
    }

    /// Get a leaf at the given index
    pub fn get_leaf(&self, index: usize) -> Option<&Hash> {
        self.leaves.get(&index)
    }
}

impl Default for MerkleTree {
    fn default() -> Self {
        Self::new()
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
    fn test_empty_tree() {
        let tree = MerkleTree::new();
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        assert_eq!(tree.height(), TREE_HEIGHT);
    }

    #[test]
    fn test_insert_single_leaf() {
        let mut tree = MerkleTree::new();
        let leaf = Hash::new([1u8; 32]);

        let index = tree.insert(leaf).unwrap();
        assert_eq!(index, 0);
        assert_eq!(tree.len(), 1);
        assert!(!tree.is_empty());
    }

    #[test]
    fn test_insert_multiple_leaves() {
        let mut tree = MerkleTree::new();

        for i in 0..10 {
            let leaf = Hash::new([i as u8; 32]);
            let index = tree.insert(leaf).unwrap();
            assert_eq!(index, i);
        }

        assert_eq!(tree.len(), 10);
    }

    #[test]
    fn test_root_changes_on_insert() {
        let mut tree = MerkleTree::new();
        let root1 = tree.root();

        let leaf = Hash::new([1u8; 32]);
        tree.insert(leaf).unwrap();
        let root2 = tree.root();

        assert_ne!(root1, root2);
    }

    #[test]
    fn test_root_deterministic() {
        let mut tree1 = MerkleTree::new();
        let mut tree2 = MerkleTree::new();

        for i in 0..5 {
            let leaf = Hash::new([i as u8; 32]);
            tree1.insert(leaf).unwrap();
            tree2.insert(leaf).unwrap();
        }

        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn test_proof_generation_and_verification() {
        let mut tree = MerkleTree::new();
        let leaf = Hash::new([42u8; 32]);

        let index = tree.insert(leaf).unwrap();
        let proof = tree.prove(index).unwrap();

        assert!(proof.verify());
        assert!(tree.verify(&proof));
    }

    #[test]
    fn test_multiple_proofs() {
        let mut tree = MerkleTree::new();

        for i in 0..5 {
            let leaf = Hash::new([i as u8; 32]);
            tree.insert(leaf).unwrap();
        }

        for i in 0..5 {
            let proof = tree.prove(i).unwrap();
            assert!(proof.verify());
            assert!(tree.verify(&proof));
        }
    }

    #[test]
    fn test_invalid_proof_index() {
        let tree = MerkleTree::new();
        let result = tree.prove(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_tree_capacity() {
        let tree = MerkleTree::with_height(3);
        assert_eq!(tree.capacity(), 8);
        assert_eq!(tree.height(), 3);
    }
}

