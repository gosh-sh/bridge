//! Layer hash chain generation for Circuit 2 test data.
//!
//! Generates synthetic Poseidon dense balanced Merkle trees and chain proofs
//! compatible with `gosh-dense-balanced-tree::verify_chain_of_dense_proofs`.
//!
//! The layer hash chain links consecutive Poseidon Merkle tree roots: each
//! tree has `2^TREE_DEPTH = 256` leaves; one leaf at
//! `CHAIN_LEAF_POSITION` carries the previous tree's root hash, the rest are
//! random Fr values. This is the *only* mainnet-acceptable configuration —
//! the smaller depth-2 / depth-3 / depth-4 fixtures previously used for
//! lightweight tests are no longer supported because depth changes the
//! circuit shape (constraint count → different VK/PK), and we don't want
//! test code that exercises a shape we will never deploy.
//!
//! The block-id shape is the canonical 16-leaf (depth-4) SHA-256 Merkle tree
//! from the `poseidon_profile_new` branch of `acki-nacki` — see the module
//! comment above the `block_merkle_root` helper below for the leaf layout.
//!
//! Entry points:
//! - [`build_synthetic_layer_hashes_input`] — end-to-end Circuit 2 input
//!   (preimage + 4 L0-opening siblings + chain proofs + 14-element instance
//!   vector).
//! - [`generate_layer_hash_chain_with_depth`] — lower-level chain-only builder;
//!   the only depth value supported in practice is [`TREE_DEPTH`].
//! - [`block_merkle_root`] / [`l0_opening_siblings`] — 16-leaf block-id
//!   tree helpers (usable outside the synthetic builder).

use bridge_poseidon::{poseidon_hash_bytes, LAYER_PREIMAGE_SIZE, MAX_LAYERS};
use gosh_dense_balanced_tree::{
    compute_root_native, fr_to_bytes, preprocess_dense_proof, DenseChainLink,
    MAX_CHAIN_LEN as DENSE_MAX_CHAIN_LEN,
};
use halo2_base::halo2_proofs::halo2curves::{bn256::Fr, group::ff::PrimeField};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Mainnet-fixed Poseidon dense-balanced-tree depth
/// (`HISTORY_PROOF_WINDOW_SIZE = 128` ⇒ 128+2 = 130 leaves ⇒ padded to
/// 2^8 = 256 ⇒ depth 8). The smaller depth-2/3/4 fixtures previously used
/// for lightweight tests are not exposed: production must run with depth 8.
pub const TREE_DEPTH: usize = 8;

/// Leaf position where the chain value (previous tree's root) is embedded.
pub const CHAIN_LEAF_POSITION: usize = 2;

/// Maximum chain length, mirroring `gosh-dense-balanced-tree::MAX_CHAIN_LEN`.
pub const MAX_CHAIN_LEN: usize = DENSE_MAX_CHAIN_LEN;

/// Number of SHA-256 Merkle siblings for the L0 opening path in the
/// depth-4 (16-leaf) block-id tree. Mirrors
/// `historical_layer_hashes_movement_checker_circuit::NUM_MERKLE_SIBLINGS`.
///
/// L0 (Poseidon of the 331-byte layer-hashes preimage) sits at leaf index 0;
/// the four opaque 32-byte siblings walked up the tree are, level by level:
///   `[L1, sha_pair(L2,L3), subtree(L4..L7), subtree(L8..L15)]`.
pub const NUM_MERKLE_SIBLINGS: usize = 4;

/// Total number of leaves in the block-id SHA-256 Merkle tree (poseidon_profile_new
/// canonical shape: 16 leaves, depth 4).
pub const BLOCK_ID_TREE_LEAF_COUNT: usize = 16;

/// Depth of the block-id SHA-256 Merkle tree.
pub const BLOCK_ID_TREE_DEPTH: usize = 4;

/// Leaf position Circuit 2 opens: L0 = Poseidon(layer-hashes preimage).
pub const L0_LEAF_INDEX: usize = 0;

/// Number of public instances Circuit 2 emits:
/// `block_id + bk_set_poseidon + num_layers + 10 layer hashes + prev_max_level_layer_hash = 14`.
pub const LAYER_HASHES_NUM_PUBLIC_INPUTS: usize = 1 + 1 + 1 + MAX_LAYERS + 1;

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Generate a random Fr value as 32-byte LE representation.
fn random_fr_bytes() -> [u8; 32] {
    use rand::RngCore;
    // Fr modulus is ~254 bits; clearing the top 3 bits keeps the result
    // well under the modulus without bias issues for test data.
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[31] &= 0x1F;
    bytes
}

/// Convert up to 32 LE bytes to Fr via inner product with powers of 256.
/// Native counterpart of the in-circuit `gate.inner_product(bytes, [256^i])`.
fn bytes_le_to_fr(bytes: &[u8]) -> Fr {
    assert!(bytes.len() <= 32);
    let mut result = Fr::zero();
    let mut power = Fr::one();
    let base = Fr::from(256u64);
    for &byte in bytes {
        result += Fr::from(byte as u64) * power;
        power *= base;
    }
    result
}

// ---------------------------------------------------------------------------
// Block-id SHA-256 Merkle tree (poseidon_profile_new canonical shape)
// ---------------------------------------------------------------------------
//
// The block-id tree is a depth-4 SHA-256 Merkle tree over 16 leaves:
//
//   L0  = Poseidon(layer_hashes_preimage)     ← the leaf Circuit 2 opens
//   L1  = SHA-256(bincode(CommonSection))
//   L2  = old_bk_set_hash                     ([0u8;32] if no BK-set change)
//   L3  = new_bk_set_hash                     ([0u8;32] if no BK-set change)
//   L4  = TVM block representation hash
//   L5  = SHA-256(bincode(durable_state_update))
//   L6  = SHA-256(tx_cnt.to_be_bytes())
//   L7  = Poseidon dense-Merkle root of [parent_block_id, refs...]
//   L8  = tracked_ext_out_messages_root       ([0u8;32] if no tracked msgs)
//   L9..L15 = [0u8; 32] (protocol-fixed zero padding)
//
// Combine rule at every internal level: `SHA-256(left_32B || right_32B)`.
// The `block_merkle_root` walker mirrors the recursive
// `block_merkle_subtree_root` implementation on the `poseidon_profile_new`
// branch of `acki-nacki` (see `helpers/proof_helper/src/gql_proof.rs`).
//
// Circuit 2 only opens L0, so the sibling path it needs is:
//   sibling[0] = L1                          (level 0: pair with L0)
//   sibling[1] = sha_pair(L2, L3)            (level 1)
//   sibling[2] = subtree(L4..L7)             (level 2)
//   sibling[3] = subtree(L8..L15)            (level 3)
//
// L1..L15 are treated as opaque bytes by Circuit 2 — no semantic checks
// beyond L0. This is why our synthetic construction lets L1..L8 be random
// and L9..L15 be zero.

fn sha256_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Recursive SHA-256 Merkle root over `leaves[start..start+width]`.
/// Mirrors `block_merkle_subtree_root` on `poseidon_profile_new`.
fn block_merkle_subtree_root(
    leaves: &[[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT],
    start: usize,
    width: usize,
) -> [u8; 32] {
    match width {
        1 => leaves[start],
        2 => sha256_pair(&leaves[start], &leaves[start + 1]),
        4 | 8 | 16 => {
            let half = width / 2;
            let left = block_merkle_subtree_root(leaves, start, half);
            let right = block_merkle_subtree_root(leaves, start + half, half);
            sha256_pair(&left, &right)
        }
        _ => unreachable!("width must be a power of 2 in [1,16]"),
    }
}

/// SHA-256 Merkle root of the depth-4 block-id tree.
pub fn block_merkle_root(leaves: &[[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT]) -> [u8; 32] {
    block_merkle_subtree_root(leaves, 0, BLOCK_ID_TREE_LEAF_COUNT)
}

/// Extract the 4 opaque siblings Circuit 2 needs to open L0 up to the
/// block-id root: `[L1, sha_pair(L2,L3), subtree(L4..L7), subtree(L8..L15)]`.
pub fn l0_opening_siblings(
    leaves: &[[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT],
) -> [[u8; 32]; NUM_MERKLE_SIBLINGS] {
    [
        leaves[1],
        block_merkle_subtree_root(leaves, 2, 2),
        block_merkle_subtree_root(leaves, 4, 4),
        block_merkle_subtree_root(leaves, 8, 8),
    ]
}

// ---------------------------------------------------------------------------
// Poseidon Merkle tree (off-circuit, native) — production depth only
// ---------------------------------------------------------------------------

/// Build a Poseidon Merkle tree with a specific chain leaf value using the given tree depth.
///
/// Builds the full tree using the same Poseidon hashing as `gosh-dense-balanced-tree`
/// (via `preprocess_dense_proof` / `compute_root_native`), ensuring byte-identical
/// results with the in-circuit verification.
fn build_tree_with_chain_leaf_depth(
    chain_value: [u8; 32],
    tree_depth: usize,
) -> ([u8; 32], Vec<[u8; 32]>) {
    let num_leaves = 1usize << tree_depth;
    let chain_pos = CHAIN_LEAF_POSITION.min(num_leaves - 1);
    let mut leaves = vec![[0u8; 32]; num_leaves];
    for (i, leaf) in leaves.iter_mut().enumerate() {
        if i == chain_pos {
            *leaf = chain_value;
        } else {
            *leaf = random_fr_bytes();
        }
    }

    // Build tree bottom-up using gosh-dense-balanced-tree's Poseidon hash.
    // This is `poseidon_hash_native(&[chunk0, chunk1, chunk2])` where chunks
    // come from `bytes_to_fr(left || right)` decomposition.
    let total_nodes = (1 << (tree_depth + 1)) - 1;
    let mut nodes = vec![[0u8; 32]; total_nodes];
    let leaf_start = (1 << tree_depth) - 1;
    for (i, leaf) in leaves.iter().enumerate() {
        nodes[leaf_start + i] = *leaf;
    }
    for i in (0..leaf_start).rev() {
        let left = nodes[2 * i + 1];
        let right = nodes[2 * i + 2];
        // Use preprocess_dense_proof with depth=1 to compute the parent hash
        // exactly as the circuit does: decompose left||right into 3 Fr chunks.
        let proof = preprocess_dense_proof(left, &[right], 0);
        let parent_fr = compute_root_native(&proof);
        nodes[i] = fr_to_bytes(parent_fr);
    }

    let root = nodes[0];

    // Extract siblings for the chain leaf position
    let mut siblings = Vec::with_capacity(tree_depth);
    let mut idx = leaf_start + chain_pos;
    for _ in 0..tree_depth {
        let sibling_idx = if idx % 2 == 1 { idx + 1 } else { idx - 1 };
        siblings.push(nodes[sibling_idx]);
        idx = (idx - 1) / 2;
    }

    // Verify: preprocess_dense_proof with these siblings should give the same root
    let proof = preprocess_dense_proof(chain_value, &siblings, chain_pos);
    let verify_root = fr_to_bytes(compute_root_native(&proof));
    assert_eq!(root, verify_root, "Tree root mismatch with preprocess_dense_proof");

    (root, siblings)
}

/// Generate a layer hash chain with explicit tree depth.
///
/// `tree_depth` MUST be [`TREE_DEPTH`] for mainnet-shape circuit
/// inputs; the parameter is retained so callers can be explicit about which
/// shape they're targeting, but no smaller value is currently a valid
/// production configuration.
pub fn generate_layer_hash_chain_with_depth(
    num_layers: usize,
    num_prev_chain_steps: usize,
    tree_depth: usize,
) -> LayerHashChainData {
    assert!(num_layers >= 1 && num_layers <= MAX_LAYERS);
    assert!(
        num_prev_chain_steps + 1 <= MAX_CHAIN_LEN,
        "total chain steps must fit in MAX_CHAIN_LEN"
    );
    assert!(tree_depth >= 1);

    let mut prev_root = random_fr_bytes();
    let initial_prev_root = prev_root;
    let total_active_steps = num_prev_chain_steps + 1;
    let chain_pos = CHAIN_LEAF_POSITION.min((1usize << tree_depth) - 1);

    let mut chain_proofs = Vec::with_capacity(MAX_CHAIN_LEN);

    for _ in 0..total_active_steps {
        let (root, siblings) = build_tree_with_chain_leaf_depth(prev_root, tree_depth);

        chain_proofs.push(ChainProofStep {
            active: true,
            siblings,
            position: chain_pos,
            leaf_value: prev_root,
        });

        prev_root = root;
    }

    let last_root = prev_root;
    for _ in total_active_steps..MAX_CHAIN_LEN {
        chain_proofs.push(ChainProofStep {
            active: false,
            siblings: vec![[0u8; 32]; tree_depth],
            position: 0,
            leaf_value: last_root,
        });
    }

    let mut root_hashes = [[0u8; 32]; MAX_LAYERS];
    for i in 0..num_layers {
        if i == num_layers - 1 {
            root_hashes[i] = prev_root;
        } else {
            root_hashes[i] = random_fr_bytes();
        }
    }

    LayerHashChainData {
        root_hashes,
        num_layers,
        prev_max_level_layer_hash: initial_prev_root,
        num_prev_chain_steps,
        chain_proofs,
    }
}

// ---------------------------------------------------------------------------
// Chain proof types
// ---------------------------------------------------------------------------

/// One step in a layer hash chain proof.
///
/// Compatible with `gosh-dense-balanced-tree::DenseChainLink`.
#[derive(Clone, Debug)]
pub struct ChainProofStep {
    /// Whether this step is active (contains a real proof).
    pub active: bool,
    /// Sibling hashes for the Merkle proof (bottom-up), one per tree level.
    pub siblings: Vec<[u8; 32]>,
    /// Leaf position in the tree.
    pub position: usize,
    /// Leaf value (32 bytes) -- the previous tree's root hash.
    pub leaf_value: [u8; 32],
}

/// Complete layer hash chain data for Circuit 2 test input.
#[derive(Clone, Debug)]
pub struct LayerHashChainData {
    /// Root hash for each of the `MAX_LAYERS` layer slots.
    /// Active layers (i < num_layers) have real Poseidon Merkle roots.
    /// Inactive layers have [0u8; 32].
    pub root_hashes: [[u8; 32]; MAX_LAYERS],
    /// Number of active layers (1..=MAX_LAYERS).
    pub num_layers: usize,
    /// The Poseidon root of the last tree in the previous chain
    /// (the starting point for this chain).
    pub prev_max_level_layer_hash: [u8; 32],
    /// Number of active chain steps (previous chain steps before current).
    pub num_prev_chain_steps: usize,
    /// Chain proof steps (exactly MAX_CHAIN_LEN entries; inactive ones are padded).
    pub chain_proofs: Vec<ChainProofStep>,
}

// ---------------------------------------------------------------------------
// Synthetic Circuit 2 end-to-end input builder
// ---------------------------------------------------------------------------
//
// `build_synthetic_layer_hashes_input` produces a fully-consistent Circuit 2
// input (layer-hashes preimage + Merkle siblings + chain proofs + expected
// 14-element public-instance vector) for a `(num_layers, num_chain_steps)`
// pair. It is the upstream replacement for the partner orchestrator's local
// `layer_hashes_test_data.rs`, and is fixed to production tree depth (8).

/// Result of [`build_synthetic_layer_hashes_input`].
pub struct SyntheticLayerHashesInput {
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub bk_set_poseidon_hash: Fr,
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: Vec<DenseChainLink>,
    pub expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

fn build_chain_step(cur_bytes: [u8; 32], depth: usize) -> (DenseChainLink, [u8; 32]) {
    let mut siblings = Vec::with_capacity(depth);
    for _ in 0..depth {
        siblings.push(random_fr_bytes());
    }
    let position = CHAIN_LEAF_POSITION.min((1 << depth) - 1);

    let proof = preprocess_dense_proof(cur_bytes, &siblings, position);
    let root_fr = compute_root_native(&proof);
    let root_bytes = fr_to_bytes(root_fr);

    let link = DenseChainLink {
        active: true,
        siblings,
        position,
        leaf_native: cur_bytes,
    };
    (link, root_bytes)
}

/// Build a synthetic, end-to-end-consistent Circuit 2 input for a given
/// `(num_layers, num_chain_steps)` pair, fixed to production tree depth 8.
///
/// All `MAX_LAYERS` slots and the full `MAX_CHAIN_LEN`-sized chain are filled
/// (with zero/inactive padding for the unused entries) so the resulting
/// circuit-builder shape is identical regardless of the chosen
/// `num_layers` / `num_chain_steps` — a single VK / PK works across all
/// combinations.
pub fn build_synthetic_layer_hashes_input(
    num_layers: usize,
    num_chain_steps: usize,
) -> SyntheticLayerHashesInput {
    assert!(num_layers >= 1 && num_layers <= MAX_LAYERS);
    assert!(
        num_chain_steps >= 1 && num_chain_steps + 1 <= MAX_CHAIN_LEN,
        "total chain steps must fit in MAX_CHAIN_LEN"
    );

    // 1. Build chain.
    let mut cur_bytes = random_fr_bytes();
    let prev_hash_fr = bytes_le_to_fr(&cur_bytes);

    let mut chain_links = Vec::with_capacity(MAX_CHAIN_LEN);
    for _ in 0..num_chain_steps {
        let (link, root_bytes) = build_chain_step(cur_bytes, TREE_DEPTH);
        chain_links.push(link);
        cur_bytes = root_bytes;
    }
    let chain_result_bytes = cur_bytes;
    let chain_result_fr = bytes_le_to_fr(&chain_result_bytes);

    let final_bytes = chain_result_bytes;
    for _ in num_chain_steps..MAX_CHAIN_LEN {
        chain_links.push(DenseChainLink::inactive(final_bytes, TREE_DEPTH));
    }

    // 2. Build layer-hashes preimage (331 bytes).
    let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
    preimage[0] = num_layers as u8;
    let mut layer_hash_frs: Vec<Fr> = Vec::with_capacity(MAX_LAYERS);
    for i in 0..MAX_LAYERS {
        let layer_number = (i + 1) as u8;
        preimage[1 + i * 33] = layer_number;
        if i < num_layers {
            if i == num_layers - 1 {
                let offset = 1 + i * 33 + 1;
                preimage[offset..offset + 32].copy_from_slice(&chain_result_bytes);
                layer_hash_frs.push(chain_result_fr);
            } else {
                let random_hash = random_fr_bytes();
                let offset = 1 + i * 33 + 1;
                preimage[offset..offset + 32].copy_from_slice(&random_hash);
                layer_hash_frs.push(bytes_le_to_fr(&random_hash));
            }
        } else {
            layer_hash_frs.push(Fr::zero());
        }
    }

    // 3. L0 = Poseidon(preimage).
    let l0_hash = poseidon_hash_bytes(&preimage);
    let l0_fr = Fr::from_repr(l0_hash).unwrap();
    let l0_bytes: [u8; 32] = l0_fr.to_repr();

    // 4. Build the 16-leaf depth-4 block-id SHA-256 Merkle tree.
    //    - L0 (index 0) = poseidon(preimage) — the one leaf Circuit 2 opens.
    //    - L1..L8       = random 32-byte values (opaque to Circuit 2).
    //    - L9..L15      = [0u8; 32] protocol-fixed zero padding.
    //    Extract the 4-sibling opening path for L0; SHA-256 output is
    //    big-endian, so reverse to LE before packing to Fr.
    let mut leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
    leaves[L0_LEAF_INDEX] = l0_bytes;
    {
        use rand::RngCore;
        let mut rng = rand::thread_rng();
        // L1..L8 randomised; L9..L15 stay at [0u8; 32] per protocol.
        for leaf in leaves.iter_mut().take(9).skip(1) {
            rng.fill_bytes(leaf);
        }
    }

    let siblings = l0_opening_siblings(&leaves);
    let root_be = block_merkle_root(&leaves);
    let mut root_le = root_be;
    root_le.reverse();
    let block_id_fr = bytes_le_to_fr(&root_le);

    // 5. Pass-through BK set commitment (the circuit doesn't recompute it for
    //    Circuit 2).
    let bk_set_poseidon_hash = Fr::from(0xDEADBEEFu64);

    // 6. Assemble the 14 expected public instances in circuit-emit order.
    let mut instances = Vec::with_capacity(LAYER_HASHES_NUM_PUBLIC_INPUTS);
    instances.push(block_id_fr);
    instances.push(bk_set_poseidon_hash);
    instances.push(Fr::from(num_layers as u64));
    instances.extend_from_slice(&layer_hash_frs);
    instances.push(prev_hash_fr);
    let expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS] = instances
        .try_into()
        .expect("instance count must equal LAYER_HASHES_NUM_PUBLIC_INPUTS");

    SyntheticLayerHashesInput {
        layer_hashes_preimage: preimage,
        merkle_siblings: siblings,
        bk_set_poseidon_hash,
        prev_max_level_layer_hash: prev_hash_fr,
        num_prev_chain_steps: num_chain_steps as u8,
        prev_chain_proofs: chain_links,
        expected_instances,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_layer_hash_chain_with_depth_basic() {
        let data = generate_layer_hash_chain_with_depth(3, 2, TREE_DEPTH);
        assert_eq!(data.num_layers, 3);
        assert_eq!(data.num_prev_chain_steps, 2);
        assert_eq!(data.chain_proofs.len(), MAX_CHAIN_LEN);

        // First 3 steps should be active (2 prev + 1 current)
        for i in 0..3 {
            assert!(data.chain_proofs[i].active);
            assert_eq!(data.chain_proofs[i].siblings.len(), TREE_DEPTH);
            assert_eq!(
                data.chain_proofs[i].position,
                CHAIN_LEAF_POSITION.min((1 << TREE_DEPTH) - 1)
            );
        }

        // Remaining should be inactive
        for i in 3..MAX_CHAIN_LEN {
            assert!(!data.chain_proofs[i].active);
        }

        // Active layers have non-zero roots
        for i in 0..3 {
            assert_ne!(data.root_hashes[i], [0u8; 32]);
        }
        // Inactive layers are zero
        for i in 3..MAX_LAYERS {
            assert_eq!(data.root_hashes[i], [0u8; 32]);
        }
    }

    #[test]
    fn test_generate_layer_hash_chain_with_depth_single_step() {
        let data = generate_layer_hash_chain_with_depth(1, 0, TREE_DEPTH);
        assert_eq!(data.num_layers, 1);
        assert_eq!(data.num_prev_chain_steps, 0);

        // Only 1 active step
        assert!(data.chain_proofs[0].active);
        for i in 1..MAX_CHAIN_LEN {
            assert!(!data.chain_proofs[i].active);
        }
    }

    #[test]
    fn test_chain_proof_step_leaf_linkage() {
        let data = generate_layer_hash_chain_with_depth(2, 3, TREE_DEPTH);
        // First step's leaf should be the initial prev_root
        assert_eq!(data.chain_proofs[0].leaf_value, data.prev_max_level_layer_hash);
    }

    #[test]
    fn test_build_synthetic_layer_hashes_input_shape() {
        let input = build_synthetic_layer_hashes_input(5, 3);
        assert_eq!(input.prev_chain_proofs.len(), MAX_CHAIN_LEN);
        // All chain proofs at production depth.
        for link in &input.prev_chain_proofs {
            assert_eq!(link.siblings.len(), TREE_DEPTH);
        }
        // 14-element public-instance vector.
        assert_eq!(input.expected_instances.len(), LAYER_HASHES_NUM_PUBLIC_INPUTS);
        // num_layers byte at preimage[0].
        assert_eq!(input.layer_hashes_preimage[0], 5);
        // Depth-4 block-id opening: exactly 4 sibling entries.
        assert_eq!(input.merkle_siblings.len(), NUM_MERKLE_SIBLINGS);
        assert_eq!(NUM_MERKLE_SIBLINGS, BLOCK_ID_TREE_DEPTH);
    }

    #[test]
    fn test_block_merkle_root_matches_opening_path() {
        // Root computed by the top-level helper must equal the value obtained
        // by walking the L0 opening path with the extracted siblings.
        let mut leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        for (i, leaf) in leaves.iter_mut().enumerate().take(9) {
            leaf[0] = (i as u8) + 1; // L0..L8 distinct non-zero
        }
        // L9..L15 remain [0u8; 32].
        let siblings = l0_opening_siblings(&leaves);
        let mut acc = leaves[L0_LEAF_INDEX];
        for sib in &siblings {
            acc = sha256_pair(&acc, sib);
        }
        assert_eq!(acc, block_merkle_root(&leaves));
    }

    #[test]
    fn test_block_merkle_root_padding_leaves_are_zero() {
        // With L0..L8 = 0 and L9..L15 = 0, the whole tree is deterministic
        // (a fully-zero-padded 16-leaf SHA-256 tree). Verifies that L9..L15
        // being zero is preserved by the recursive walker.
        let leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        // Level 1
        let n01 = sha256_pair(&leaves[0], &leaves[1]);
        let n23 = sha256_pair(&leaves[2], &leaves[3]);
        let n45 = sha256_pair(&leaves[4], &leaves[5]);
        let n67 = sha256_pair(&leaves[6], &leaves[7]);
        let n89 = sha256_pair(&leaves[8], &leaves[9]);
        let n10_11 = sha256_pair(&leaves[10], &leaves[11]);
        let n12_13 = sha256_pair(&leaves[12], &leaves[13]);
        let n14_15 = sha256_pair(&leaves[14], &leaves[15]);
        // Level 2
        let n0_3 = sha256_pair(&n01, &n23);
        let n4_7 = sha256_pair(&n45, &n67);
        let n8_11 = sha256_pair(&n89, &n10_11);
        let n12_15 = sha256_pair(&n12_13, &n14_15);
        // Level 3
        let n0_7 = sha256_pair(&n0_3, &n4_7);
        let n8_15 = sha256_pair(&n8_11, &n12_15);
        // Root
        let expected = sha256_pair(&n0_7, &n8_15);
        assert_eq!(block_merkle_root(&leaves), expected);
    }
}
