//! Poseidon Merkle chain proof construction for Circuit 2.
//!
//! Builds DenseChainLink witnesses connecting prev_max_level_layer_hash
//! to the highest active layer hash in the new key block.
//!
//! The chain operates at the layer-hash level: each step proves that a value
//! is a leaf in a Poseidon Merkle tree whose root is the next value in the chain.

use gosh_dense_balanced_tree::{
    compute_root_native, fr_to_bytes, preprocess_dense_proof, DenseChainLink, MAX_CHAIN_LEN,
};
use tvm_vm::executor::zk_stuff::bn254::poseidon::PoseidonSponge;

/// Data needed to build one layer hash tree for chain proof construction.
#[derive(Clone, Debug)]
pub struct LayerTreeData {
    /// All leaves of the Poseidon Merkle tree (must be power of 2).
    /// For layer 1: [higher_root, prev_same_root, block_leaf_0, ..., block_leaf_{n-2}]
    /// For layer 2+: [higher_root, prev_same_root, layer_N-1_hash_0, ...]
    pub leaves: Vec<[u8; 32]>,
    /// Position of the chain leaf (the previous root value) in the tree.
    /// Typically 1 (second leaf = prev_same_layer_root).
    pub chain_leaf_position: usize,
    /// The chain leaf value (prev_same_layer_root).
    pub chain_leaf_value: [u8; 32],
}

/// Compute a block leaf hash: Poseidon(block_id || envelope_hash || ext_messages_root).
///
/// Uses the node's own PoseidonSponge (from tvm_vm) to guarantee byte-identical
/// results with the node's `compute_block_leaf_hash` in history_proof.rs.
pub fn compute_block_leaf_hash(
    block_id: &[u8; 32],
    envelope_hash: &[u8; 32],
    ext_messages_root: &[u8; 32],
) -> [u8; 32] {
    let sponge = tvm_vm::executor::zk_stuff::bn254::poseidon::PoseidonSponge::new();
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(block_id);
    buf[32..64].copy_from_slice(envelope_hash);
    buf[64..96].copy_from_slice(ext_messages_root);
    sponge.hash_bytes_flat(&buf).expect("Poseidon hash failed")
}

/// Combine two 32-byte children into a parent using the node's PoseidonSponge.
/// This matches the node's `dense_combine(hasher, left, right)`.
fn node_dense_combine(sponge: &PoseidonSponge, left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    sponge.hash_bytes_flat(&buf).expect("Poseidon hash failed")
}

/// Build a Poseidon Merkle tree from leaves and extract a Merkle proof for a given position.
///
/// Uses the node's PoseidonSponge for tree combine to ensure byte-identical
/// results with the acki-nacki node's `dense_merkle_tree`.
///
/// Returns (root_bytes, siblings) where siblings is bottom-up.
pub fn build_tree_and_proof(
    leaves: &[[u8; 32]],
    proof_position: usize,
) -> ([u8; 32], Vec<[u8; 32]>) {
    let num_leaves = leaves.len();
    assert!(num_leaves.is_power_of_two() && num_leaves >= 2);
    let depth = num_leaves.trailing_zeros() as usize;

    let sponge = PoseidonSponge::new();

    let total_nodes = (1 << (depth + 1)) - 1;
    let mut nodes = vec![[0u8; 32]; total_nodes];
    let leaf_start = (1 << depth) - 1;

    // Place leaves
    for (i, leaf) in leaves.iter().enumerate() {
        nodes[leaf_start + i] = *leaf;
    }

    // Build internal nodes bottom-up using node's PoseidonSponge
    for i in (0..leaf_start).rev() {
        let left = nodes[2 * i + 1];
        let right = nodes[2 * i + 2];
        nodes[i] = node_dense_combine(&sponge, &left, &right);
    }

    let root = nodes[0];

    // Extract siblings for proof_position
    let mut siblings = Vec::with_capacity(depth);
    let mut idx = leaf_start + proof_position;
    for _ in 0..depth {
        let sibling_idx = if idx % 2 == 1 { idx + 1 } else { idx - 1 };
        siblings.push(nodes[sibling_idx]);
        idx = (idx - 1) / 2;
    }

    // Verify: gosh-dense-balanced-tree proof should give the same root
    // (needed because the circuit uses gosh-dense-balanced-tree for verification)
    let proof = preprocess_dense_proof(leaves[proof_position], &siblings, proof_position);
    let verify_root = fr_to_bytes(compute_root_native(&proof));
    if root != verify_root {
        tracing::warn!(
            "Node PoseidonSponge root differs from gosh-dense-balanced-tree root!\n  node: {}\n  gosh: {}",
            hex::encode(root), hex::encode(verify_root)
        );
    }

    (root, siblings)
}

/// Build a chain of DenseChainLinks from intermediate key block tree data.
///
/// Each element of `trees` describes one step: a Poseidon Merkle tree that contains
/// the previous value as a leaf. The chain starts from `prev_hash` and should arrive
/// at the root of the last tree.
///
/// Returns (chain_links padded to MAX_CHAIN_LEN, num_active_steps).
pub fn build_chain_proofs(
    trees: &[LayerTreeData],
) -> (Vec<DenseChainLink>, u8) {
    let num_steps = trees.len();
    assert!(
        num_steps >= 1 && num_steps <= MAX_CHAIN_LEN,
        "chain steps must be 1..={}",
        MAX_CHAIN_LEN
    );

    let mut chain_links = Vec::with_capacity(MAX_CHAIN_LEN);
    let mut last_root_bytes = [0u8; 32];

    for tree_data in trees {
        let (root, siblings) = build_tree_and_proof(
            &tree_data.leaves,
            tree_data.chain_leaf_position,
        );

        chain_links.push(DenseChainLink {
            active: true,
            siblings,
            position: tree_data.chain_leaf_position,
            leaf_native: tree_data.chain_leaf_value,
        });

        last_root_bytes = root;
    }

    // Pad with inactive links
    let tree_depth = if trees.is_empty() {
        1
    } else {
        trees[0].leaves.len().trailing_zeros() as usize
    };
    for _ in num_steps..MAX_CHAIN_LEN {
        chain_links.push(DenseChainLink::inactive(last_root_bytes, tree_depth));
    }

    (chain_links, num_steps as u8)
}

/// Pad leaves to the next power of 2 with zero-bytes.
pub fn pad_leaves_to_power_of_2(leaves: &mut Vec<[u8; 32]>) {
    let n = leaves.len();
    if n.is_power_of_two() {
        return;
    }
    let next_pow2 = n.next_power_of_two();
    leaves.resize(next_pow2, [0u8; 32]);
}
