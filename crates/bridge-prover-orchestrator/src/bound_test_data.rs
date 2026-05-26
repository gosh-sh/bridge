//! Phase 4 — Cross-circuit bound test data.
//!
//! Wraps the partner's [`generate_bridge_test_data`] so all three live
//! verifiers (Circuit 1A primary, Circuit 1B fallback, Circuit 2
//! layer-hashes-movement) consume **the same** synthetic block scenario and
//! therefore emit `(block_id, bk_set_poseidon)` public-input pairs that match
//! byte-for-byte.
//!
//! That cross-circuit consistency is the whole reason
//! `AckiNackiBridge.verifyBlock` can safely accept a tuple of two independent
//! Halo2 SHPLONK / gnark Groth16 proofs and treat them as describing the same
//! AN block: if 1A's `block_id` disagrees with 2's, the bridge reverts with
//! `BlockIdMismatch`. Foundry tests that exercise that revert path need bound
//! fixtures to assert the bridge accepts the matching case at all.
//!
//! Two helpers:
//!
//! - [`build_bound_test_data`] — one-shot generator that produces the primary
//!   attestation, the Circuit 2 layer-hashes witness, and (optionally) a
//!   fallback attestation envelope signed over the same `block_id`.
//! - [`compose_layer_hashes_input`] — slot-fill the Circuit 2
//!   [`crate::LayerHashesProofInput`] from a [`BoundBlockTestData`].
//!
//! Note: `generate_bridge_test_data` uses `rand::thread_rng()` internally, so
//! the data is non-deterministic across runs. The export binary captures the
//! generated public inputs in JSON and reuses them in Foundry fixtures; we
//! never need byte-level reproducibility, only same-process consistency.

use std::collections::HashMap;

use anyhow::Context;
use bridge_test_data_gen::{
    bls::{Secret, SignerIndex},
    envelope_hash::poseidon_hash_bytes,
    generator::{
        create_attestation_data, generate_bridge_test_data, sign_attestation_multi, BridgeTestData,
    },
    layer_hashes::ChainProofStep,
    types::AttestationTargetType,
};
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use historical_layer_hashes_movement_checker_circuit::{
    test_helpers::bytes_le_to_fr, LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
};

use crate::layer_hashes_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS;

/// Cross-circuit bound block test data.
///
/// Produced by [`build_bound_test_data`]. Holds everything Circuit 1A / 1B / 2
/// need plus the derived public-instance vectors the orchestrator will hand to
/// the on-chain verifiers and the relayer will hand to AckiNackiBridge.
#[derive(Clone)]
pub struct BoundBlockTestData {
    // ---- Shared (cross-circuit) ----
    pub block_id_bytes: [u8; 32],
    pub block_id_fr: Fr,
    pub bk_set: HashMap<SignerIndex, Vec<u8>>,
    pub bk_set_poseidon_fr: Fr,
    pub block_seq_no: u32,
    pub last_seen_block_seqno: u32,

    // ---- Attestations ----
    pub attestation_primary_bytes: Vec<u8>,
    /// Fallback attestation envelope signed by every keypair over the same
    /// `block_id` as the primary, target_type = Fallback. Only populated when
    /// `with_fallback = true` is passed to [`build_bound_test_data`].
    pub attestation_fallback_bytes: Option<Vec<u8>>,

    // ---- Circuit 2 inputs ----
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: Vec<DenseChainLink>,
    pub layer_hash_frs: [Fr; MAX_LAYERS],
    pub num_layers: u32,
    pub layer_hashes_expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

impl BoundBlockTestData {
    /// Public-instance vector emitted by Circuit 1A or 1B for this scenario.
    /// `[block_id, bk_set_poseidon, block_seq_no, last_seen]`.
    pub fn attestation_instances(&self) -> [Fr; 4] {
        [
            self.block_id_fr,
            self.bk_set_poseidon_fr,
            Fr::from(self.block_seq_no as u64),
            Fr::from(self.last_seen_block_seqno as u64),
        ]
    }

    /// Public-instance vector emitted by Circuit 2 for this scenario.
    pub fn layer_hashes_instances(&self) -> [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS] {
        self.layer_hashes_expected_instances
    }
}

/// Build a bound block scenario.
///
/// Wraps [`bridge_test_data_gen::generator::generate_bridge_test_data`] and:
/// 1. derives the Poseidon BK-set commitment via the partner's
///    `bridge_poseidon::compute_bk_set_poseidon`;
/// 2. computes the 14 Circuit 2 public instances from the chain data;
/// 3. converts `ChainProofStep` (partner) → `DenseChainLink`
///    (gosh-dense-balanced-tree) so the orchestrator's existing prover can
///    consume it without changes;
/// 4. when `with_fallback = true`, additionally signs a Fallback attestation
///    over the same `block_id` with all keypairs.
///
/// Constraints (mirrored from `generate_bridge_test_data`):
/// - `bk_set_size >= 2`;
/// - `1 <= num_layers <= 10`;
/// - `1 <= num_prev_chain_steps`, `num_prev_chain_steps + 1 <= MAX_CHAIN_LEN =
///   11`.
pub fn build_bound_test_data(
    bk_set_size: usize,
    num_layers: usize,
    num_prev_chain_steps: usize,
    with_fallback: bool,
) -> anyhow::Result<BoundBlockTestData> {
    let td = generate_bridge_test_data(bk_set_size, num_layers, num_prev_chain_steps)
        .context("partner generate_bridge_test_data failed")?;

    Ok(promote_bridge_test_data(td, with_fallback)?)
}

/// Convert a partner [`BridgeTestData`] into the bound shape, including all
/// derived Fr instances. Split out so a caller can drive the partner's
/// generator with custom parameters and then promote.
pub fn promote_bridge_test_data(
    td: BridgeTestData,
    with_fallback: bool,
) -> anyhow::Result<BoundBlockTestData> {
    let block_id_bytes = td.block_id;
    let block_id_fr = bytes_le_to_fr(&block_id_bytes);

    // BK-set Poseidon commitment (matches what Circuit 1A/1B and Circuit 2
    // emit as public input [1]). The partner's `compute_bk_set_poseidon`
    // returns the same value the circuits enforce.
    let (bk_set_poseidon_fr, _bk_set_poseidon_bytes) =
        bridge_poseidon::compute_bk_set_poseidon(&td.bk_set);

    let block_seq_no = extract_block_seq_no(&td.attestation_bytes);
    let last_seen_block_seqno = block_seq_no
        .checked_sub(1)
        .context("synthetic block_seq_no must be >= 1")?;

    // ---- Circuit 2 layer-hashes preimage ----
    let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
    let preimage_src = &td.layer_hashes_preimage;
    debug_assert_eq!(preimage_src.len(), LAYER_PREIMAGE_SIZE);
    preimage.copy_from_slice(preimage_src);

    // Sanity: l0 = Poseidon(preimage) must be the first leaf of the Merkle tree
    // (the partner's generator constructs it that way; we re-check rather than
    // trust a comment).
    let l0_recomputed = poseidon_hash_bytes(&preimage);
    debug_assert_eq!(
        l0_recomputed, td.merkle_tree.leaves[0],
        "BUG: layer_hashes_preimage Poseidon must equal Merkle leaf L0"
    );

    let merkle_siblings = td.merkle_tree.siblings_for_l0();

    // ---- Layer hash Fr per slot ----
    let mut layer_hash_frs = [Fr::zero(); MAX_LAYERS];
    for (i, lh) in layer_hash_frs.iter_mut().enumerate() {
        *lh = bytes_le_to_fr(&td.layer_hash_chain.root_hashes[i]);
    }

    let prev_max_level_layer_hash = bytes_le_to_fr(&td.layer_hash_chain.prev_max_level_layer_hash);
    let num_prev_chain_steps = td.layer_hash_chain.num_prev_chain_steps as u8;

    let prev_chain_proofs = td
        .layer_hash_chain
        .chain_proofs
        .iter()
        .map(chain_step_to_dense_link)
        .collect::<Vec<_>>();

    // ---- Circuit 2 expected instances (14) ----
    let mut instances: Vec<Fr> = Vec::with_capacity(LAYER_HASHES_NUM_PUBLIC_INPUTS);
    instances.push(block_id_fr);
    instances.push(bk_set_poseidon_fr);
    instances.push(Fr::from(td.layer_hash_chain.num_layers as u64));
    instances.extend_from_slice(&layer_hash_frs);
    instances.push(prev_max_level_layer_hash);
    let layer_hashes_expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS] =
        instances.try_into().expect("instance count mismatch");

    // ---- Optional fallback attestation ----
    let attestation_fallback_bytes = if with_fallback {
        Some(build_fallback_attestation_envelope(&td)?)
    } else {
        None
    };

    Ok(BoundBlockTestData {
        block_id_bytes,
        block_id_fr,
        bk_set: td.bk_set,
        bk_set_poseidon_fr,
        block_seq_no,
        last_seen_block_seqno,
        attestation_primary_bytes: td.attestation_bytes,
        attestation_fallback_bytes,
        layer_hashes_preimage: preimage,
        merkle_siblings,
        prev_max_level_layer_hash,
        num_prev_chain_steps,
        prev_chain_proofs,
        layer_hash_frs,
        num_layers: td.layer_hash_chain.num_layers as u32,
        layer_hashes_expected_instances,
    })
}

/// Build a Fallback attestation envelope (`target_type = Fallback`) signed by
/// every BK-set keypair over the same `block_id` carried by
/// `td.attestation_bytes`.
///
/// Mirrors the Primary attestation construction in `generate_bridge_test_data`
/// step 12 — reuses `td.keypairs` (the original signers; the partner's
/// generator appends one extra keypair after the BK set was modified, which we
/// must skip here so the fallback signs with exactly the *current* set).
fn build_fallback_attestation_envelope(td: &BridgeTestData) -> anyhow::Result<Vec<u8>> {
    // td.keypairs has one extra entry at the end (the new signer for the BK
    // change scenario). Use only the original `bk_set.len()` signers.
    let original_signer_count = td.bk_set.len();
    let signers: Vec<(SignerIndex, &Secret)> = td
        .keypairs
        .iter()
        .take(original_signer_count)
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();

    let attestation_data = create_attestation_data(td.block_id, AttestationTargetType::Fallback);
    let envelope = sign_attestation_multi(attestation_data, &signers)
        .context("signing fallback attestation failed")?;
    let bytes = bincode::serialize(&envelope).context("bincoding fallback envelope failed")?;
    Ok(bytes)
}

/// Convert the partner's [`ChainProofStep`] into the orchestrator-facing
/// [`DenseChainLink`]. Layouts match field-for-field (active, siblings,
/// position, leaf bytes); only the leaf-field name differs (`leaf_value` vs
/// `leaf_native`).
fn chain_step_to_dense_link(step: &ChainProofStep) -> DenseChainLink {
    DenseChainLink {
        active: step.active,
        siblings: step.siblings.clone(),
        position: step.position,
        leaf_native: step.leaf_value,
    }
}

/// Compose the Circuit 2
/// [`LayerHashesProofInput`](crate::LayerHashesProofInput)
/// from a [`BoundBlockTestData`]. Trivial slot-filling helper kept here so
/// callers don't need to know the field-name mapping.
pub fn compose_layer_hashes_input<'a>(
    bound: &'a BoundBlockTestData,
) -> crate::LayerHashesProofInput<'a> {
    crate::LayerHashesProofInput {
        layer_hashes_preimage: bound.layer_hashes_preimage,
        merkle_siblings: bound.merkle_siblings,
        prev_max_level_layer_hash: bound.prev_max_level_layer_hash,
        num_prev_chain_steps: bound.num_prev_chain_steps,
        prev_chain_proofs: &bound.prev_chain_proofs,
        bk_set_poseidon_hash: bound.bk_set_poseidon_fr,
        expected_instances: bound.layer_hashes_expected_instances,
    }
}

/// Mirror of `prover.rs::extract_block_seq_no`. Re-implemented to keep this
/// module's surface independent of the prover's private helpers.
fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
