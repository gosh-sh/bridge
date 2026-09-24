//! Shared SHA-256 / Poseidon helpers and the layer-hashes preimage builder.
//!
//! The canonical 16-leaf, depth-4 block-id Merkle tree is built with
//! [`crate::layer_hashes::block_merkle_root`] /
//! [`crate::layer_hashes::l0_opening_siblings`]; see
//! `GLOBAL_HISTORY_DATA_SPEC_MULTITHREAD.md` for the leaf layout.

use sha2::{Digest, Sha256};

// Re-export Poseidon functions and constants from bridge-poseidon (single source of truth).
pub use bridge_poseidon::{
    poseidon_hash_bytes, poseidon_hash_fr, poseidon_hash_fr_to_bytes,
    compute_bk_set_poseidon, compute_bk_set_poseidon_padded_to,
    decompose_pubkey_x_to_limbs,
    POSEIDON_T, POSEIDON_RATE, POSEIDON_R_F, POSEIDON_R_P,
    LIMB_BITS, NUM_LIMBS, MAX_SIGNERS, PADDING_SIGNER_INDEX,
    MAX_LAYERS, LAYER_PREIMAGE_SIZE,
};

// ---------------------------------------------------------------------------
// SHA-256 helpers
// ---------------------------------------------------------------------------

/// SHA-256(left || right) for Merkle internal nodes.
pub fn sha256_combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// SHA-256 of arbitrary bytes.
pub fn sha256_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// Layer hashes preimage
// ---------------------------------------------------------------------------

/// Build layer hashes preimage (331 bytes, fixed size).
///
/// Format:
/// ```text
/// [1B]   num_layers (u8, 1..=10; 0 if no history proofs yet)
/// For i in 1..=10:
///   [1B]   layer_number (u8, value = i)
///   [32B]  root_hash    (Poseidon Merkle root of that layer; [0u8; 32] if inactive)
/// ```
pub fn build_layer_hashes_preimage(
    num_layers: usize,
    root_hashes: &[[u8; 32]; MAX_LAYERS],
) -> Vec<u8> {
    assert!(num_layers <= MAX_LAYERS);
    let mut preimage = Vec::with_capacity(LAYER_PREIMAGE_SIZE);
    preimage.push(num_layers as u8);
    for i in 0..MAX_LAYERS {
        preimage.push((i + 1) as u8); // layer_number = i+1
        if i < num_layers {
            preimage.extend_from_slice(&root_hashes[i]);
        } else {
            preimage.extend_from_slice(&[0u8; 32]);
        }
    }
    assert_eq!(preimage.len(), LAYER_PREIMAGE_SIZE);
    preimage
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

    #[test]
    fn test_sha256_combine() {
        let left = [0xAAu8; 32];
        let right = [0xBBu8; 32];
        let result = sha256_combine(&left, &right);
        assert_ne!(result, [0u8; 32]);
        assert_eq!(result, sha256_combine(&left, &right));
        assert_ne!(result, sha256_combine(&right, &left));
    }

    #[test]
    fn test_poseidon_hash_bytes_deterministic() {
        let data = vec![1u8; 331];
        let h1 = poseidon_hash_bytes(&data);
        let h2 = poseidon_hash_bytes(&data);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn test_poseidon_hash_bytes_331() {
        let data = vec![0x42u8; 331];
        let h = poseidon_hash_bytes(&data);
        assert_ne!(h, [0u8; 32]);
    }

    #[test]
    fn test_build_layer_hashes_preimage_size() {
        let root_hashes = [[0xFFu8; 32]; MAX_LAYERS];
        let preimage = build_layer_hashes_preimage(5, &root_hashes);
        assert_eq!(preimage.len(), LAYER_PREIMAGE_SIZE);
        assert_eq!(preimage[0], 5);
        for i in 0..MAX_LAYERS {
            assert_eq!(preimage[1 + i * 33], (i + 1) as u8);
        }
    }

    #[test]
    fn test_build_layer_hashes_preimage_inactive_zeroed() {
        let mut root_hashes = [[0u8; 32]; MAX_LAYERS];
        root_hashes[0] = [0xAAu8; 32];
        root_hashes[1] = [0xBBu8; 32];
        let preimage = build_layer_hashes_preimage(2, &root_hashes);
        assert_eq!(&preimage[2..34], &[0xAAu8; 32]);
        assert_eq!(&preimage[35..67], &[0xBBu8; 32]);
        for i in 2..MAX_LAYERS {
            let offset = 1 + i * 33 + 1;
            assert_eq!(&preimage[offset..offset + 32], &[0u8; 32]);
        }
    }

    #[test]
    fn test_decompose_pubkey_x_to_limbs() {
        let (_sk, pk) = crate::bls::gen_keypair();
        let pk_bytes = pk.to_bytes();
        let limbs = decompose_pubkey_x_to_limbs(&pk_bytes);
        let non_zero_count = limbs.iter().filter(|l| **l != Fr::zero()).count();
        assert!(non_zero_count >= 2, "Expected at least 2 non-zero limbs for a real pubkey");
    }

    #[test]
    fn test_compute_bk_set_poseidon_deterministic() {
        let keypairs = crate::generator::generate_bls_keypairs(3);
        let bk_set = crate::generator::build_bk_set_map(&keypairs);
        let (fr1, bytes1) = compute_bk_set_poseidon_padded_to(&bk_set, MAX_SIGNERS);
        let (fr2, bytes2) = compute_bk_set_poseidon_padded_to(&bk_set, MAX_SIGNERS);
        assert_eq!(fr1, fr2);
        assert_eq!(bytes1, bytes2);
        assert_ne!(bytes1, [0u8; 32]);
    }
}
