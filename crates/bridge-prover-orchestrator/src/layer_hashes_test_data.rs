//! Synthetic Circuit 2 test-data builder.
//!
//! Mirrors `real_prover.rs::build_test_case` from
//! `historical-layer-hashes-movement-checker-circuit/tests/` byte-for-byte,
//! exposed here as a reusable library function so both the orchestrator's
//! round-trip integration test and the `export-layer-hashes-proof` binary
//! drive identical inputs without copying ~150 lines of generator code.

use gosh_dense_balanced_tree::{
    compute_root_native, fr_to_bytes, preprocess_dense_proof, DenseChainLink, MAX_CHAIN_LEN,
};
use halo2_base::halo2_proofs::halo2curves::{bn256::Fr, group::ff::PrimeField};
use historical_layer_hashes_movement_checker_circuit::{
    test_helpers::bytes_le_to_fr, LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::layer_hashes_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS;

/// Tree depth for the synthetic Poseidon Merkle chain.
///
/// Mirrors the partner's `tests/real_prover.rs::TREE_DEPTH = 4` (lightweight
/// configuration for `HISTORY_PROOF_WINDOW_SIZE = 8`). Production deployment
/// uses depth 8 (`WINDOW_SIZE = 128`); we'll re-fixture later.
const TREE_DEPTH: usize = 4;

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

fn random_fr_bytes() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b[31] &= 0x1F;
    b
}

fn build_chain_step(cur_bytes: [u8; 32], depth: usize) -> (DenseChainLink, [u8; 32]) {
    let mut siblings = Vec::with_capacity(depth);
    for _ in 0..depth {
        siblings.push(random_fr_bytes());
    }
    let position = 2usize.min((1 << depth) - 1);

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
/// `(num_layers, num_chain_steps)` pair. All MAX_LAYERS slots and the full
/// MAX_CHAIN_LEN-sized chain are filled (with zero/inactive padding for the
/// inactive entries) so the resulting circuit-builder shape is identical
/// regardless of the chosen `num_layers` / `num_chain_steps` — that's why a
/// single VK / PK works across all combinations.
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
    let l0_hash = bridge_poseidon::poseidon_hash_bytes(&preimage);
    let l0_fr = Fr::from_repr(l0_hash).unwrap();
    let l0_bytes: [u8; 32] = l0_fr.to_repr();

    // 4. Random SHA-256 Merkle siblings + reconstruct block_id root.
    let siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS] = {
        let mut s = [[0u8; 32]; NUM_MERKLE_SIBLINGS];
        for sib in s.iter_mut() {
            rand::thread_rng().fill_bytes(sib);
        }
        s
    };

    let h0: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(l0_bytes);
        h.update(siblings[0]);
        h.finalize().into()
    };
    let h01: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(h0);
        h.update(siblings[1]);
        h.finalize().into()
    };
    let root_be: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(h01);
        h.update(siblings[2]);
        h.finalize().into()
    };
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
