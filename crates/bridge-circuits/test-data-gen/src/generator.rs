use std::collections::HashMap;

use crate::bls::gen_keypair;
use crate::bls::PubKey;
use crate::bls::Secret;
use crate::bls::SignerIndex;
use crate::envelope_hash::{
    build_layer_hashes_preimage, poseidon_hash_bytes, sha256_hash,
};
use crate::layer_hashes::{
    block_merkle_root, generate_layer_hash_chain_with_depth, l0_opening_siblings,
    LayerHashChainData, BLOCK_ID_TREE_LEAF_COUNT, NUM_MERKLE_SIBLINGS, TREE_DEPTH,
};
use crate::types::AckiNackiEnvelopeHash;
use crate::types::AttestationData;
use crate::types::AttestationTargetType;
use crate::types::BlockIdentifier;
use crate::types::BlockSeqNo;
use crate::types::Envelope;

/// All generated test data for attestation BLS verification.
pub struct TestData {
    /// Keypairs: (secret, pubkey, signer_index)
    pub keypairs: Vec<(Secret, PubKey, SignerIndex)>,
    /// BK set: signer_index -> 48-byte compressed pubkey
    pub bk_set: HashMap<SignerIndex, Vec<u8>>,
    /// Serialized Envelope<AttestationData> (raw bytes)
    pub attestation_bytes: Vec<u8>,
    /// Second attestation for fallback finalization (None for primary-only tests)
    pub attestation_2_bytes: Option<Vec<u8>>,
    /// The block_id embedded in the attestation (random for synthetic tests)
    pub block_id: [u8; 32],
}

/// Generate BLS keypairs, returning (secret, pubkey, signer_index).
pub fn generate_bls_keypairs(n: usize) -> Vec<(Secret, PubKey, SignerIndex)> {
    (0..n)
        .map(|i| {
            let (secret, pubkey) = gen_keypair();
            (secret, pubkey, i as SignerIndex)
        })
        .collect()
}

/// Build a BK set map from keypairs: signer_index -> 48-byte compressed pubkey.
pub fn build_bk_set_map(keypairs: &[(Secret, PubKey, SignerIndex)]) -> HashMap<SignerIndex, Vec<u8>> {
    keypairs
        .iter()
        .map(|(_, pk, idx)| (*idx, pk.to_bytes().to_vec()))
        .collect()
}

/// Create AttestationData with the given block_id Merkle root.
///
/// The `block_id_hash` is the 16-leaf (depth-4) SHA-256 Merkle root (block identifier).
/// The `envelope_hash` field gets a random value (circuits use block_id, not envelope_hash).
pub fn create_attestation_data(
    block_id_hash: [u8; 32],
    target_type: AttestationTargetType,
) -> AttestationData {
    let mut parent_id_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut parent_id_bytes);
    let mut envelope_hash_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut envelope_hash_bytes);

    AttestationData {
        parent_block_id: BlockIdentifier(parent_id_bytes),
        block_id: BlockIdentifier(block_id_hash),
        block_seq_no: BlockSeqNo::from(1),
        envelope_hash: AckiNackiEnvelopeHash(envelope_hash_bytes),
        target_type,
    }
}

/// Sign attestation with multiple signers, producing Envelope<AttestationData>.
pub fn sign_attestation_multi(
    data: AttestationData,
    signers: &[(SignerIndex, &Secret)],
) -> anyhow::Result<Envelope<AttestationData>> {
    assert!(!signers.is_empty(), "Need at least one signer");
    let (first_idx, first_secret) = signers[0];
    let mut envelope = Envelope::sealed(data, first_secret, first_idx)?;
    for &(idx, secret) in &signers[1..] {
        envelope.add_signature(idx, secret)?;
    }
    Ok(envelope)
}

/// Generate a random 32-byte envelope hash for synthetic tests.
fn random_block_id() -> [u8; 32] {
    let mut hash = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut hash);
    hash
}

/// Generate BLS keypairs with custom (non-contiguous) signer indices.
pub fn generate_bls_keypairs_with_indices(indices: &[SignerIndex]) -> Vec<(Secret, PubKey, SignerIndex)> {
    indices
        .iter()
        .map(|&idx| {
            let (secret, pubkey) = gen_keypair();
            (secret, pubkey, idx)
        })
        .collect()
}

/// Generate test data with non-contiguous BK set indices, all sign.
///
/// Uses the provided `indices` as signer indices instead of 0..n.
/// Exercises the index-remapping logic that maps protocol-level indices
/// to sorted array positions.
pub fn generate_test_data_all_sign_custom_indices(indices: &[SignerIndex]) -> anyhow::Result<TestData> {
    assert!(!indices.is_empty(), "need at least 1 signer");

    let keypairs = generate_bls_keypairs_with_indices(indices);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    let attestation_data = create_attestation_data(block_id, AttestationTargetType::Primary);

    let attestation_signers: Vec<(SignerIndex, &Secret)> = keypairs
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let attestation_envelope = sign_attestation_multi(attestation_data, &attestation_signers)?;
    let attestation_bytes = bincode::serialize(&attestation_envelope)?;

    let att_pubkeys: Vec<(PubKey, usize)> = keypairs
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(
            &attestation_envelope.aggregated_signature,
            &att_pubkeys,
            &attestation_envelope.data,
        )?,
        "Attestation BLS signature verification failed!"
    );

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: None,
        block_id,
    })
}

/// Generate test data where ALL members of the BK set sign the attestation.
///
/// - Creates `bk_set_size` keypairs (signers 0..bk_set_size-1)
/// - Random block_id (Circuit 1 doesn't verify it against block data)
/// - Attestation signed by every signer in the BK set
pub fn generate_test_data_all_sign(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 1, "need at least 1 signer");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    let attestation_data = create_attestation_data(block_id, AttestationTargetType::Primary);

    let attestation_signers: Vec<(SignerIndex, &Secret)> = keypairs
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let attestation_envelope = sign_attestation_multi(attestation_data, &attestation_signers)?;
    let attestation_bytes = bincode::serialize(&attestation_envelope)?;

    // Verify BLS signature off-circuit.
    let att_pubkeys: Vec<(PubKey, usize)> = keypairs
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(
            &attestation_envelope.aggregated_signature,
            &att_pubkeys,
            &attestation_envelope.data,
        )?,
        "Attestation BLS signature verification failed!"
    );

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: None,
        block_id,
    })
}

/// Generate test data with exactly ceil(2n/3) signers (Primary threshold minimum).
pub fn generate_test_data_primary_threshold(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 1, "need at least 1 signer");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    let attestation_data = create_attestation_data(block_id, AttestationTargetType::Primary);

    // Primary threshold: ceil(2n/3)
    let num_signers = (2 * bk_set_size + 2) / 3; // ceil(2n/3)
    let attestation_signers: Vec<(SignerIndex, &Secret)> = keypairs[..num_signers]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let attestation_envelope = sign_attestation_multi(attestation_data, &attestation_signers)?;
    let attestation_bytes = bincode::serialize(&attestation_envelope)?;

    // Verify off-circuit.
    let att_pubkeys: Vec<(PubKey, usize)> = keypairs[..num_signers]
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(
            &attestation_envelope.aggregated_signature,
            &att_pubkeys,
            &attestation_envelope.data,
        )?,
        "Attestation BLS signature verification failed!"
    );

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: None,
        block_id,
    })
}

/// Generate test data with only floor(n/2)+1 signers — Primary type but below 2/3 threshold.
/// Circuit should FAIL verification.
pub fn generate_test_data_primary_below_threshold(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 2, "need at least 2 signers for below-threshold test");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    let attestation_data = create_attestation_data(block_id, AttestationTargetType::Primary);

    // Only floor(n/2)+1 signers — not enough for Primary (needs ceil(2n/3))
    let num_signers = bk_set_size / 2 + 1;
    let attestation_signers: Vec<(SignerIndex, &Secret)> = keypairs[..num_signers]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let attestation_envelope = sign_attestation_multi(attestation_data, &attestation_signers)?;
    let attestation_bytes = bincode::serialize(&attestation_envelope)?;

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: None,
        block_id,
    })
}

/// Generate fallback test data where ALL members sign both attestations.
///
/// - att1: Primary type, all signers
/// - att2: Fallback type, all signers
/// - Same block_id for both
pub fn generate_test_data_fallback_all_sign(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 1, "need at least 1 signer");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    let all_signers: Vec<(SignerIndex, &Secret)> = keypairs
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();

    // Attestation 1: Primary type, all sign.
    let att_data_1 = create_attestation_data(block_id, AttestationTargetType::Primary);
    let att_envelope_1 = sign_attestation_multi(att_data_1, &all_signers)?;
    let attestation_bytes = bincode::serialize(&att_envelope_1)?;

    // Attestation 2: Fallback type, all sign.
    let att_data_2 = create_attestation_data(block_id, AttestationTargetType::Fallback);
    let att_envelope_2 = sign_attestation_multi(att_data_2, &all_signers)?;
    let attestation_2_bytes = bincode::serialize(&att_envelope_2)?;

    // Verify both BLS signatures off-circuit.
    let att_pubkeys: Vec<(PubKey, usize)> = keypairs
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(&att_envelope_1.aggregated_signature, &att_pubkeys, &att_envelope_1.data)?,
        "Primary attestation BLS verification failed!"
    );
    assert!(
        crate::bls::verify(&att_envelope_2.aggregated_signature, &att_pubkeys, &att_envelope_2.data)?,
        "Fallback attestation BLS verification failed!"
    );

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: Some(attestation_2_bytes),
        block_id,
    })
}

/// Generate fallback test data with floor(n/2)+1 signers per attestation (just above 50%).
///
/// - att1: Primary type, first floor(n/2)+1 signers
/// - att2: Fallback type, last floor(n/2)+1 signers
/// - Same block_id for both
pub fn generate_test_data_fallback_threshold(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 2, "need at least 2 signers for threshold test");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    // Fallback threshold: > 50%, so floor(n/2)+1 is the minimum.
    let num_signers = bk_set_size / 2 + 1;

    // Attestation 1: Primary type, first num_signers keypairs.
    let signers_1: Vec<(SignerIndex, &Secret)> = keypairs[..num_signers]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let att_data_1 = create_attestation_data(block_id, AttestationTargetType::Primary);
    let att_envelope_1 = sign_attestation_multi(att_data_1, &signers_1)?;
    let attestation_bytes = bincode::serialize(&att_envelope_1)?;

    // Attestation 2: Fallback type, last num_signers keypairs.
    let signers_2: Vec<(SignerIndex, &Secret)> = keypairs[bk_set_size - num_signers..]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let att_data_2 = create_attestation_data(block_id, AttestationTargetType::Fallback);
    let att_envelope_2 = sign_attestation_multi(att_data_2, &signers_2)?;
    let attestation_2_bytes = bincode::serialize(&att_envelope_2)?;

    // Verify both BLS signatures off-circuit.
    let att_pubkeys_1: Vec<(PubKey, usize)> = keypairs[..num_signers]
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(&att_envelope_1.aggregated_signature, &att_pubkeys_1, &att_envelope_1.data)?,
        "Primary attestation BLS verification failed!"
    );
    let att_pubkeys_2: Vec<(PubKey, usize)> = keypairs[bk_set_size - num_signers..]
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(&att_envelope_2.aggregated_signature, &att_pubkeys_2, &att_envelope_2.data)?,
        "Fallback attestation BLS verification failed!"
    );

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: Some(attestation_2_bytes),
        block_id,
    })
}

/// Generate fallback test data with only floor(n/2) signers per attestation (<=50%).
/// Circuit should FAIL verification (needs >50%).
pub fn generate_test_data_fallback_below_threshold(bk_set_size: usize) -> anyhow::Result<TestData> {
    assert!(bk_set_size >= 4, "need at least 4 signers for below-threshold fallback test");

    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);
    let block_id = random_block_id();

    // floor(n/2) signers — not enough for Fallback (needs >50%)
    let num_signers = bk_set_size / 2;

    // Attestation 1: Primary type, first num_signers.
    let signers_1: Vec<(SignerIndex, &Secret)> = keypairs[..num_signers]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let att_data_1 = create_attestation_data(block_id, AttestationTargetType::Primary);
    let att_envelope_1 = sign_attestation_multi(att_data_1, &signers_1)?;
    let attestation_bytes = bincode::serialize(&att_envelope_1)?;

    // Attestation 2: Fallback type, last num_signers.
    let signers_2: Vec<(SignerIndex, &Secret)> = keypairs[bk_set_size - num_signers..]
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let att_data_2 = create_attestation_data(block_id, AttestationTargetType::Fallback);
    let att_envelope_2 = sign_attestation_multi(att_data_2, &signers_2)?;
    let attestation_2_bytes = bincode::serialize(&att_envelope_2)?;

    Ok(TestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: Some(attestation_2_bytes),
        block_id,
    })
}

// ===========================================================================
// Circuit 2 integrated test data (canonical 16-leaf, depth-4 block-id tree)
// ===========================================================================

/// Complete test data for Circuit 2 (Layer Hashes Prover).
///
/// Includes the 16-leaf, depth-4 SHA-256 block-id Merkle tree (see
/// `GLOBAL_HISTORY_DATA_SPEC_MULTITHREAD.md`), the layer-hash chain, and the
/// data needed for the circuit's private witnesses and public instances.
///
/// Circuit 2 only opens leaf L0 (= `Poseidon(layer_hashes_preimage)`); the
/// remaining leaves L1..L15 are opaque here — the tree is walked with the
/// 4 top-level SHA-256 siblings returned by [`l0_opening_siblings`].
pub struct BridgeTestData {
    /// Keypairs: (secret, pubkey, signer_index)
    pub keypairs: Vec<(Secret, PubKey, SignerIndex)>,
    /// BK set: signer_index -> 48-byte compressed pubkey
    pub bk_set: HashMap<SignerIndex, Vec<u8>>,
    /// Serialized Envelope<AttestationData> (raw bytes)
    pub attestation_bytes: Vec<u8>,
    /// Second attestation for fallback finalization (None for primary-only tests)
    pub attestation_2_bytes: Option<Vec<u8>>,
    /// The block_id = 16-leaf depth-4 SHA-256 Merkle root.
    pub block_id: [u8; 32],

    // ---- Block-id Merkle tree (16 leaves, depth 4) ----

    /// All 16 leaves L0..L15 of the block-id tree. L0 is
    /// `Poseidon(layer_hashes_preimage)`; L9..L15 are protocol-fixed zero.
    pub block_merkle_leaves: [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT],
    /// The 4 opaque SHA-256 siblings Circuit 2 witnesses when opening L0.
    pub l0_opening_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],

    // ---- Circuit 2 data ----

    /// Layer hashes preimage (331 bytes).
    pub layer_hashes_preimage: Vec<u8>,
    /// Layer hash chain data (root hashes, chain proofs, prev hash).
    pub layer_hash_chain: LayerHashChainData,
}

/// Generate a random 32-byte value by SHA-256-hashing random input.
fn random_sha256_leaf(data_len: usize) -> [u8; 32] {
    let mut data = vec![0u8; data_len];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut data);
    sha256_hash(&data)
}

/// Generate complete test data for Circuit 2 with the canonical 16-leaf,
/// depth-4 SHA-256 block-id Merkle tree.
///
/// Builds:
/// 1. BLS keypairs and BK set
/// 2. Layer hash chain (Poseidon dense Merkle trees at production TREE_DEPTH)
/// 3. Layer hashes preimage (331 bytes)
/// 4. All 16 block-id-tree leaves (L0 = `Poseidon(preimage)`; L1..L8 random;
///    L9..L15 zero as required by the canonical spec) and the 4 top-level
///    SHA-256 siblings needed to open L0
/// 5. Attestation signed with the Merkle root as `block_id`
///
/// Parameters:
/// - `bk_set_size`: number of signers in the BK set (>= 1)
/// - `num_layers`: number of active layers (1..=10)
/// - `num_prev_chain_steps`: number of previous chain steps before current
pub fn generate_bridge_test_data(
    bk_set_size: usize,
    num_layers: usize,
    num_prev_chain_steps: usize,
) -> anyhow::Result<BridgeTestData> {
    assert!(bk_set_size >= 1, "need at least 1 signer");
    assert!(num_layers >= 1 && num_layers <= 10);

    // 1. Generate keypairs and BK set.
    let keypairs = generate_bls_keypairs(bk_set_size);
    let bk_set = build_bk_set_map(&keypairs);

    // 2. Generate layer hash chain at production tree depth (the only
    //    mainnet-acceptable shape; smaller depths are deliberately not
    //    supported — see test-data-gen::layer_hashes module docs).
    let layer_hash_chain = generate_layer_hash_chain_with_depth(
        num_layers,
        num_prev_chain_steps,
        TREE_DEPTH,
    );

    // 3. Build layer hashes preimage (331 bytes).
    let layer_hashes_preimage = build_layer_hashes_preimage(
        layer_hash_chain.num_layers,
        &layer_hash_chain.root_hashes,
    );

    // 4. Compute L0 = Poseidon(preimage split into 31-byte Fr chunks).
    //    Circuit 2 opens this leaf; L1..L8 are opaque here (random SHA-256
    //    fillers stand in for the canonical protocol leaves); L9..L15 are
    //    protocol-fixed zero padding.
    let l0 = poseidon_hash_bytes(&layer_hashes_preimage);
    let l1 = random_sha256_leaf(196);
    let l2 = random_sha256_leaf(64);
    let l3 = random_sha256_leaf(64);
    let l4 = random_sha256_leaf(128);
    let l5 = random_sha256_leaf(256);
    let l6 = random_sha256_leaf(8);
    let l7 = random_sha256_leaf(128);
    let l8 = random_sha256_leaf(384);

    let mut block_merkle_leaves = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
    block_merkle_leaves[0] = l0;
    block_merkle_leaves[1] = l1;
    block_merkle_leaves[2] = l2;
    block_merkle_leaves[3] = l3;
    block_merkle_leaves[4] = l4;
    block_merkle_leaves[5] = l5;
    block_merkle_leaves[6] = l6;
    block_merkle_leaves[7] = l7;
    block_merkle_leaves[8] = l8;
    // L9..L15 already zero-initialized (protocol-fixed padding).

    // 5. Fold the 16 leaves into `block_id` and derive the 4 opaque siblings
    //    Circuit 2 witnesses when opening L0.
    let block_id = block_merkle_root(&block_merkle_leaves);
    let siblings = l0_opening_siblings(&block_merkle_leaves);

    // 6. Create attestation with block_id = block-id tree root.
    let attestation_data =
        create_attestation_data(block_id, AttestationTargetType::Primary);

    let attestation_signers: Vec<(SignerIndex, &Secret)> = keypairs
        .iter()
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();
    let attestation_envelope = sign_attestation_multi(attestation_data, &attestation_signers)?;
    let attestation_bytes = bincode::serialize(&attestation_envelope)?;

    // Verify BLS signature off-circuit.
    let att_pubkeys: Vec<(PubKey, usize)> = keypairs
        .iter()
        .map(|(_, pk, _)| (pk.clone(), 1))
        .collect();
    assert!(
        crate::bls::verify(
            &attestation_envelope.aggregated_signature,
            &att_pubkeys,
            &attestation_envelope.data,
        )?,
        "Attestation BLS signature verification failed!"
    );

    Ok(BridgeTestData {
        keypairs,
        bk_set,
        attestation_bytes,
        attestation_2_bytes: None,
        block_id,
        block_merkle_leaves,
        l0_opening_siblings: siblings,
        layer_hashes_preimage,
        layer_hash_chain,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod bridge_tests {
    use super::*;
    use crate::envelope_hash::sha256_combine;

    #[test]
    fn test_generate_bridge_test_data_basic() {
        let data = generate_bridge_test_data(5, 3, 1).unwrap();

        // Verify block_id matches folding the 16 leaves.
        assert_eq!(data.block_id, block_merkle_root(&data.block_merkle_leaves));

        // Verify preimage size.
        assert_eq!(data.layer_hashes_preimage.len(), 331);

        // Verify L0 is Poseidon of preimage.
        let l0 = poseidon_hash_bytes(&data.layer_hashes_preimage);
        assert_eq!(data.block_merkle_leaves[0], l0);

        // Verify L9..L15 are the protocol-fixed zero padding.
        for i in 9..BLOCK_ID_TREE_LEAF_COUNT {
            assert_eq!(data.block_merkle_leaves[i], [0u8; 32], "leaf L{i} must be zero");
        }
    }

    #[test]
    fn test_merkle_path_for_l0() {
        let data = generate_bridge_test_data(3, 2, 0).unwrap();
        let siblings = data.l0_opening_siblings;

        // Walk depth-4 from L0 with the 4 opaque top-level siblings.
        let mut acc = data.block_merkle_leaves[0];
        for sib in siblings.iter() {
            acc = sha256_combine(&acc, sib);
        }
        assert_eq!(acc, data.block_id);
    }

    #[test]
    fn test_layer_chain_data_consistency() {
        let data = generate_bridge_test_data(3, 5, 3).unwrap();
        let chain = &data.layer_hash_chain;

        assert_eq!(chain.num_layers, 5);
        assert_eq!(chain.num_prev_chain_steps, 3);
        assert_eq!(chain.chain_proofs.len(), crate::layer_hashes::MAX_CHAIN_LEN);

        // The highest layer's root hash should match the last chain step's tree root.
        // (This is by construction in generate_layer_hash_chain_with_depth.)
        assert_ne!(chain.root_hashes[4], [0u8; 32]); // layer 5 (index 4) active
        assert_eq!(chain.root_hashes[5], [0u8; 32]); // layer 6 inactive
    }
}
