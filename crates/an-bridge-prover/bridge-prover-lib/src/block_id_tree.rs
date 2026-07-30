//! 16-leaf, depth-4 SHA-256 Merkle tree for the canonical `block_id`
//! (acki-nacki `poseidon_profile_new`, see
//! `acki-nacki-to-eth-bridge-halo2-circuits/GLOBAL_HISTORY_DATA_SPEC_MULTITHREAD.md`).
//!
//! Reconstructs the `block_id` and extracts the Merkle siblings needed to open
//! two leaf views:
//!   1. L0 (Poseidon of the layer-hashes preimage) — 4 opaque top-level SHA
//!      siblings, matching Circuit 2 `NUM_MERKLE_SIBLINGS = 4`.
//!   2. The L2/L3 pair (old/new BK-set Poseidon commitments) — 3 sibling nodes,
//!      consumed by the no-Circuit-3 BK-set rotation path (see
//!      `docs/bk_set_update_no_circuit3_plan.md`).
//!
//! Tree structure:
//! ```text
//!                                     root  (= block_id)
//!                                    /                     \
//!                             h_0_7                            h_8_15
//!                            /      \                         /       \
//!                       h_0_3        h_4_7               h_8_11       h_12_15
//!                       /  \         /   \                /   \         /   \
//!                     h01 h23      h45   h67           h89 h10_11   h12_13 h14_15
//!                    / \  / \      / \   / \           / \   /  \    /  \   /   \
//!                   L0 L1 L2 L3   L4 L5 L6 L7         L8 L9 L10 L11 L12 L13 L14 L15
//! ```
//!
//! Internal nodes: `SHA-256(left_32B || right_32B)`. Leaf values, per the
//! canonical spec:
//! - L0 = `Poseidon(layer_hashes_preimage)` (331 bytes split into 31-byte Fr chunks)
//! - L1 = `SHA-256(bincode(CommonSection))`
//! - L2 = `Poseidon(old_bk_set_hash)` (32 bytes LE) — zero if no BK-set change
//! - L3 = `Poseidon(new_bk_set_hash)` (32 bytes LE) — zero if no BK-set change
//! - L4 = TVM block representation hash
//! - L5 = `SHA-256(bincode(durable_state_update))`
//! - L6 = `SHA-256(tx_cnt.to_be_bytes())`
//! - L7 = Poseidon Merkle root of `[parent_block_id, refs…]`
//! - L8 = `tracked_ext_out_messages_root` (event-binding leaf for Circuit 4)
//! - L9..L15 = `[0u8; 32]` — protocol-fixed zero padding
//!
//! The prover never recomputes the individual leaves; it fetches all 16 via the
//! GraphQL `block_merkle_tree_leaves` field and folds them here.

use sha2::{Digest, Sha256};

pub use historical_layer_hashes_movement_checker_circuit::NUM_MERKLE_SIBLINGS;

/// Canonical leaf count of the block-id tree (fixed by the protocol). Sourced
/// from the circuits repo (`bridge_test_data_gen::layer_hashes`) so any change
/// to the tree width propagates automatically instead of drifting across
/// duplicated `= 16` literals.
pub use bridge_test_data_gen::layer_hashes::BLOCK_ID_TREE_LEAF_COUNT;

/// Number of siblings required to open the L2/L3 pair up to `block_id`.
/// One less than the tree depth because we start from the L2/L3 pair hash.
pub const L2_L3_OPEN_SIBLINGS: usize = 3;

/// SHA-256(left || right) for Merkle internal nodes.
fn sha256_combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// All data for the 16-leaf block-id Merkle tree. Every field is materialised so
/// callers can pull whichever internal node they need without re-folding.
#[derive(Clone, Debug)]
pub struct BlockIdMerkleTree {
    /// All 16 leaves L0..L15.
    pub leaves: [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT],

    // Level 1 — pairs of leaves.
    pub h01: [u8; 32],
    pub h23: [u8; 32],
    pub h45: [u8; 32],
    pub h67: [u8; 32],
    pub h89: [u8; 32],
    pub h10_11: [u8; 32],
    pub h12_13: [u8; 32],
    pub h14_15: [u8; 32],

    // Level 2 — spans of 4 leaves.
    pub h0_3: [u8; 32],
    pub h4_7: [u8; 32],
    pub h8_11: [u8; 32],
    pub h12_15: [u8; 32],

    // Level 3 — spans of 8 leaves.
    pub h0_7: [u8; 32],
    pub h8_15: [u8; 32],

    /// Root = `block_id`.
    pub root: [u8; 32],
}

impl BlockIdMerkleTree {
    /// Build the tree from all 16 leaves.
    pub fn from_leaves(leaves: [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT]) -> Self {
        let h01 = sha256_combine(&leaves[0], &leaves[1]);
        let h23 = sha256_combine(&leaves[2], &leaves[3]);
        let h45 = sha256_combine(&leaves[4], &leaves[5]);
        let h67 = sha256_combine(&leaves[6], &leaves[7]);
        let h89 = sha256_combine(&leaves[8], &leaves[9]);
        let h10_11 = sha256_combine(&leaves[10], &leaves[11]);
        let h12_13 = sha256_combine(&leaves[12], &leaves[13]);
        let h14_15 = sha256_combine(&leaves[14], &leaves[15]);

        let h0_3 = sha256_combine(&h01, &h23);
        let h4_7 = sha256_combine(&h45, &h67);
        let h8_11 = sha256_combine(&h89, &h10_11);
        let h12_15 = sha256_combine(&h12_13, &h14_15);

        let h0_7 = sha256_combine(&h0_3, &h4_7);
        let h8_15 = sha256_combine(&h8_11, &h12_15);

        let root = sha256_combine(&h0_7, &h8_15);

        Self {
            leaves,
            h01, h23, h45, h67, h89, h10_11, h12_13, h14_15,
            h0_3, h4_7, h8_11, h12_15,
            h0_7, h8_15,
            root,
        }
    }

    /// Siblings for opening L0 up to `block_id`. Matches Circuit 2's
    /// `NUM_MERKLE_SIBLINGS = 4` witness layout: fold with
    /// `sha_pair(acc, sibling[i])` for i = 0..4 starting from `acc = L0`.
    pub fn siblings_for_l0(&self) -> [[u8; 32]; NUM_MERKLE_SIBLINGS] {
        [self.leaves[1], self.h23, self.h4_7, self.h8_15]
    }

    /// Siblings for opening the L2/L3 pair up to `block_id`. The verifier
    /// starts from `h23 = sha_pair(L2, L3)` and folds with each sibling in
    /// turn: `sha_pair(h01_sibling, h23) → h0_3`,
    /// `sha_pair(h0_3, h4_7_sibling) → h0_7`,
    /// `sha_pair(h0_7, h8_15_sibling) → root`.
    pub fn siblings_for_l2_l3(&self) -> [[u8; 32]; L2_L3_OPEN_SIBLINGS] {
        [self.h01, self.h4_7, self.h8_15]
    }

    /// Block ID = root of the tree.
    pub fn block_id(&self) -> [u8; 32] {
        self.root
    }
}

/// Re-derive `block_id` from an *open* L2/L3 pair and its three siblings —
/// the verifier's half of [`BlockIdMerkleTree::siblings_for_l2_l3`], for
/// callers that never see the other 14 leaves:
///
/// ```text
/// h23  = SHA(l2   ‖ l3)
/// h0_3 = SHA(s[0] ‖ h23)          // s[0] = h01
/// h0_7 = SHA(h0_3 ‖ s[1])         // s[1] = h4_7
/// root = SHA(h0_7 ‖ s[2])         // s[2] = h8_15
/// ```
///
/// `l2` / `l3` are the BK-set Poseidon commitments in the canonical LE `Fr`
/// repr the chain hashes into the leaves — *not* their numeric big-endian
/// image. `bridge-verifier-daemon` and `AckiNackiBridge.applyBkSetUpdate`
/// (which rebuilds the LE repr from the numeric commitment it is handed)
/// both run exactly this fold.
pub fn fold_l2_l3_open(
    l2: &[u8; 32],
    l3: &[u8; 32],
    siblings: &[[u8; 32]; L2_L3_OPEN_SIBLINGS],
) -> [u8; 32] {
    let h23 = sha256_combine(l2, l3);
    let h0_3 = sha256_combine(&siblings[0], &h23);
    let h0_7 = sha256_combine(&h0_3, &siblings[1]);
    sha256_combine(&h0_7, &siblings[2])
}

/// Build a 331-byte layer hashes preimage from layer root hashes.
///
/// Format: `[num_layers: u8] + 10 * [layer_number: u8, root_hash: [u8; 32]]`.
pub fn build_layer_hashes_preimage(
    num_layers: usize,
    root_hashes: &[[u8; 32]],
) -> [u8; 331] {
    assert!(num_layers <= 10);
    assert!(root_hashes.len() >= num_layers);

    let mut preimage = [0u8; 331];
    preimage[0] = num_layers as u8;

    for i in 0..10 {
        let offset = 1 + i * 33;
        preimage[offset] = (i + 1) as u8; // layer_number = i+1
        if i < num_layers {
            preimage[offset + 1..offset + 1 + 32].copy_from_slice(&root_hashes[i]);
        }
        // Inactive layers remain zero
    }

    preimage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_leaves() -> [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT] {
        let mut leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        for (i, leaf) in leaves.iter_mut().enumerate() {
            // Distinct, non-zero content per leaf so accidental symmetry
            // (e.g. all zeros) can't mask a bug.
            leaf[0] = (i as u8) + 1;
        }
        leaves
    }

    #[test]
    fn root_and_l0_opening_are_consistent() {
        let leaves = indexed_leaves();
        let tree = BlockIdMerkleTree::from_leaves(leaves);
        let siblings = tree.siblings_for_l0();

        // Walk depth-4 from L0.
        let mut acc = leaves[0];
        for sib in siblings.iter() {
            acc = sha256_combine(&acc, sib);
        }
        assert_eq!(acc, tree.root);
    }

    #[test]
    fn l2_l3_opening_reconstructs_root() {
        let leaves = indexed_leaves();
        let tree = BlockIdMerkleTree::from_leaves(leaves);
        let siblings = tree.siblings_for_l2_l3();

        // Start from sha(L2 ‖ L3), then fold up.
        let mut acc = sha256_combine(&leaves[2], &leaves[3]);
        acc = sha256_combine(&siblings[0], &acc); // (h01, h23)
        acc = sha256_combine(&acc, &siblings[1]); // (h0_3, h4_7)
        acc = sha256_combine(&acc, &siblings[2]); // (h0_7, h8_15)
        assert_eq!(acc, tree.root);

        // Same result through the helper the verifier daemon uses.
        assert_eq!(
            fold_l2_l3_open(&leaves[2], &leaves[3], &siblings),
            tree.root,
        );
    }

    /// Cross-language pin against `AckiNackiBridge.applyBkSetUpdate`.
    ///
    /// The contract is handed the two commitments as *numeric* field elements
    /// and rebuilds their LE repr internally (`_frToLeBytes`) before folding.
    /// This is the same vector its
    /// `test_applyBkSetUpdate_matchesOffChainVector` submits on-chain, so if
    /// either side changes fold depth, sibling order, or the endianness of the
    /// L2/L3 preimage, exactly one of the two tests goes red.
    #[test]
    fn l2_l3_opening_matches_on_chain_vector() {
        // 0xA11CE and 0xB0B as `Fr::to_repr()` would serialise them.
        let mut l2 = [0u8; 32];
        l2[..8].copy_from_slice(&0xA11CEu64.to_le_bytes());
        let mut l3 = [0u8; 32];
        l3[..8].copy_from_slice(&0xB0Bu64.to_le_bytes());
        let siblings = [[0x11u8; 32], [0x22u8; 32], [0x33u8; 32]];

        assert_eq!(
            hex::encode(fold_l2_l3_open(&l2, &l3, &siblings)),
            "28ee9f98c4d654e9e4b33712fa1652ecd9fecd2a591fac1370b1b51e2b65fba4",
        );
    }

    #[test]
    fn zero_padded_right_subtree_matches_spec() {
        // Only L0 populated; L1..L15 all zero. Ensures our fold agrees with the
        // canonical "L9..L15 = zero" collapse described in the spec.
        let mut leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        leaves[0] = [0xAB; 32];
        let tree = BlockIdMerkleTree::from_leaves(leaves);

        let expected = {
            let mut acc = leaves[0];
            let siblings = tree.siblings_for_l0();
            for sib in siblings.iter() {
                acc = sha256_combine(&acc, sib);
            }
            acc
        };
        assert_eq!(expected, tree.root);
    }
}
