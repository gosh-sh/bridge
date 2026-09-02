//! Poseidon-bn254 dense-Merkle helpers used by the GraphQL-metadata block
//! proof pathway — byte-identical to the node's `history-proof` crate. Keep
//! in sync if upstream constants change.
//!
//! Underlying primitive: `pse_poseidon::Poseidon<halo2_Fr, T=3, RATE=2>` with
//! `R_F=8, R_P=57`. Inputs are chunked into 31-byte pieces, right-padded to
//! 32 bytes with a single trailing zero, interpreted as little-endian field
//! elements, fed into the sponge, then the 32-byte LE output is returned.
//!
//! This module is layered on top of `bridge_poseidon` (the single
//! byte-hashing source of truth for the whole bridge workspace) and adds
//! the dense Poseidon Merkle-tree helpers used by Circuit 2 (history
//! layers) and Circuit 4 (event proofs).

pub const HISTORY_PROOF_WINDOW_SIZE: usize = 128;
pub type LayerNumber = u8;

/// Marker for the dense-Merkle helpers.
///
/// Byte hashing is delegated to [`bridge_poseidon::poseidon_hash_bytes`], so
/// this struct is stateless. Kept as a named type so call sites like
/// `PoseidonHasher::new()` + `hasher.digest(&buf)` and function signatures
/// like `dense_combine(&hasher, ...)` stay readable and unchanged.
#[derive(Default, Clone, Copy)]
pub struct PoseidonHasher;

impl PoseidonHasher {
    pub fn new() -> Self {
        Self
    }

    /// Poseidon-bn254 byte digest. Delegates to
    /// [`bridge_poseidon::poseidon_hash_bytes`] — this is the *single*
    /// byte-hashing surface for the whole prover workspace.
    pub fn digest(&self, bytes: &[u8]) -> [u8; 32] {
        bridge_poseidon::poseidon_hash_bytes(bytes)
    }
}

fn dense_combine(hasher: &PoseidonHasher, left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    hasher.digest(&buf)
}

fn dense_merkle_tree(hasher: &PoseidonHasher, leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    assert!(!leaves.is_empty());
    let width = leaves.len().next_power_of_two();
    let mut tree = vec![[0u8; 32]; 2 * width - 1];
    let leaf_start = width - 1;
    for (i, leaf) in leaves.iter().enumerate() {
        tree[leaf_start + i] = *leaf;
    }
    for i in (0..leaf_start).rev() {
        tree[i] = dense_combine(hasher, &tree[2 * i + 1], &tree[2 * i + 2]);
    }
    tree
}

pub fn dense_merkle_root(hasher: &PoseidonHasher, leaves: &[[u8; 32]]) -> [u8; 32] {
    dense_merkle_tree(hasher, leaves)[0]
}

pub fn dense_merkle_proof(
    hasher: &PoseidonHasher,
    leaves: &[[u8; 32]],
    pos: usize,
) -> Vec<[u8; 32]> {
    let tree = dense_merkle_tree(hasher, leaves);
    let width = leaves.len().next_power_of_two();
    let mut idx = width - 1 + pos;
    let mut proof = Vec::new();
    while idx > 0 {
        let sibling = if idx % 2 == 1 { idx + 1 } else { idx - 1 };
        proof.push(tree[sibling]);
        idx = (idx - 1) / 2;
    }
    proof
}

pub fn dense_merkle_verify(
    hasher: &PoseidonHasher,
    root: &[u8; 32],
    leaf: &[u8; 32],
    pos: usize,
    proof: &[[u8; 32]],
) -> bool {
    let width = 1usize << proof.len();
    let mut idx = width - 1 + pos;
    let mut current = *leaf;
    for sibling in proof {
        current = if idx % 2 == 1 {
            dense_combine(hasher, &current, sibling)
        } else {
            dense_combine(hasher, sibling, &current)
        };
        idx = (idx - 1) / 2;
    }
    current == *root
}

pub fn compute_block_leaf_hash(
    block_id: &[u8; 32],
    envelope_hash: &[u8; 32],
    tracked_ext_out_messages_root: &[u8; 32],
) -> [u8; 32] {
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(block_id);
    buf[32..64].copy_from_slice(envelope_hash);
    buf[64..96].copy_from_slice(tracked_ext_out_messages_root);
    PoseidonHasher::new().digest(&buf)
}

pub fn compute_ext_message_leaf_hash(
    account_dapp_id: &[u8; 32],
    account_id: &[u8; 32],
    ext_message_hash: &[u8; 32],
) -> [u8; 32] {
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(account_dapp_id);
    buf[32..64].copy_from_slice(account_id);
    buf[64..96].copy_from_slice(ext_message_hash);
    PoseidonHasher::new().digest(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Etalon vector originally captured from the node's
    /// `zk_stuff::bn254::poseidon::test_poseidon_bytes_flat` test. If this
    /// fails, our Poseidon parameters have drifted from the node and no
    /// on-chain proof will verify.
    #[test]
    fn poseidon_digest_matches_node_etalon() {
        let h = PoseidonHasher::new();
        let digest = h.digest(&vec![0xFFu8; 32]);
        let etalon: [u8; 32] = [
            17, 144, 181, 203, 195, 40, 59, 230, 38, 96, 237, 159, 26, 21, 81, 182, 3, 65, 4, 198,
            100, 165, 92, 201, 156, 197, 209, 125, 0, 99, 218, 18,
        ];
        assert_eq!(digest, etalon);
    }

    #[test]
    fn dense_merkle_proof_verifies_for_all_leaves() {
        let h = PoseidonHasher::new();
        let leaves = [[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]];
        let root = dense_merkle_root(&h, &leaves);
        for (pos, leaf) in leaves.iter().enumerate() {
            let proof = dense_merkle_proof(&h, &leaves, pos);
            assert!(dense_merkle_verify(&h, &root, leaf, pos, &proof));
        }
    }
}
