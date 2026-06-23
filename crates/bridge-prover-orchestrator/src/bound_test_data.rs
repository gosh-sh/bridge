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
//!
//! For the Poseidon re-prove pass (R15 aggregator), [`save_bound_witness_cache`]
//! / [`load_bound_witness_cache`] persist the exact witness so Phase A2 does
//! not regenerate a different random scenario.

use std::{
    collections::HashMap,
    path::Path,
};

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
use halo2_base::halo2_proofs::halo2curves::{bn256::Fr, ff::PrimeField};
use historical_layer_hashes_movement_checker_circuit::{
    test_helpers::bytes_le_to_fr, LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
};

use crate::layer_hashes_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS;

#[derive(serde::Serialize, serde::Deserialize)]
struct DenseChainLinkCache {
    active: bool,
    siblings: Vec<[u8; 32]>,
    position: usize,
    leaf_native: [u8; 32],
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BoundWitnessCache {
    block_id_bytes: [u8; 32],
    block_id_fr: [u8; 32],
    bk_set: HashMap<SignerIndex, Vec<u8>>,
    bk_set_poseidon_fr: [u8; 32],
    block_seq_no: u32,
    last_seen_block_seqno: u32,
    attestation_primary_bytes: Vec<u8>,
    attestation_fallback_bytes: Option<Vec<u8>>,
    layer_hashes_preimage: Vec<u8>,
    merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    prev_max_level_layer_hash: [u8; 32],
    num_prev_chain_steps: u8,
    prev_chain_proofs: Vec<DenseChainLinkCache>,
    layer_hash_frs: [[u8; 32]; MAX_LAYERS],
    num_layers: u32,
    layer_hashes_expected_instances: [[u8; 32]; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

fn fr_to_bytes(fr: Fr) -> [u8; 32] {
    let repr = fr.to_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(repr.as_ref());
    out
}

fn bytes_to_fr(bytes: [u8; 32]) -> Fr {
    let mut repr = <Fr as PrimeField>::Repr::default();
    repr.as_mut().copy_from_slice(&bytes);
    Fr::from_repr(repr).expect("invalid Fr in bound witness cache")
}

impl BoundWitnessCache {
    pub fn from_bound(bound: &BoundBlockTestData) -> Self {
        Self {
            block_id_bytes: bound.block_id_bytes,
            block_id_fr: fr_to_bytes(bound.block_id_fr),
            bk_set: bound.bk_set.clone(),
            bk_set_poseidon_fr: fr_to_bytes(bound.bk_set_poseidon_fr),
            block_seq_no: bound.block_seq_no,
            last_seen_block_seqno: bound.last_seen_block_seqno,
            attestation_primary_bytes: bound.attestation_primary_bytes.clone(),
            attestation_fallback_bytes: bound.attestation_fallback_bytes.clone(),
            layer_hashes_preimage: bound.layer_hashes_preimage.to_vec(),
            merkle_siblings: bound.merkle_siblings,
            prev_max_level_layer_hash: fr_to_bytes(bound.prev_max_level_layer_hash),
            num_prev_chain_steps: bound.num_prev_chain_steps,
            prev_chain_proofs: bound
                .prev_chain_proofs
                .iter()
                .map(|link| DenseChainLinkCache {
                    active: link.active,
                    siblings: link.siblings.clone(),
                    position: link.position,
                    leaf_native: link.leaf_native,
                })
                .collect(),
            layer_hash_frs: bound.layer_hash_frs.map(fr_to_bytes),
            num_layers: bound.num_layers,
            layer_hashes_expected_instances: bound.layer_hashes_expected_instances.map(fr_to_bytes),
        }
    }

    pub fn into_bound(self) -> BoundBlockTestData {
        BoundBlockTestData {
            block_id_bytes: self.block_id_bytes,
            block_id_fr: bytes_to_fr(self.block_id_fr),
            bk_set: self.bk_set,
            bk_set_poseidon_fr: bytes_to_fr(self.bk_set_poseidon_fr),
            block_seq_no: self.block_seq_no,
            last_seen_block_seqno: self.last_seen_block_seqno,
            attestation_primary_bytes: self.attestation_primary_bytes,
            attestation_fallback_bytes: self.attestation_fallback_bytes,
            layer_hashes_preimage: {
                let mut a = [0u8; LAYER_PREIMAGE_SIZE];
                a.copy_from_slice(&self.layer_hashes_preimage);
                a
            },
            merkle_siblings: self.merkle_siblings,
            prev_max_level_layer_hash: bytes_to_fr(self.prev_max_level_layer_hash),
            num_prev_chain_steps: self.num_prev_chain_steps,
            prev_chain_proofs: self
                .prev_chain_proofs
                .into_iter()
                .map(|link| DenseChainLink {
                    active: link.active,
                    siblings: link.siblings,
                    position: link.position,
                    leaf_native: link.leaf_native,
                })
                .collect(),
            layer_hash_frs: self.layer_hash_frs.map(bytes_to_fr),
            num_layers: self.num_layers,
            layer_hashes_expected_instances: self.layer_hashes_expected_instances.map(bytes_to_fr),
        }
    }
}

pub fn save_bound_witness_cache(bound: &BoundBlockTestData, path: &Path) -> anyhow::Result<()> {
    let bytes = bincode::serialize(&BoundWitnessCache::from_bound(bound))?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn load_bound_witness_cache(path: &Path) -> anyhow::Result<BoundBlockTestData> {
    let bytes = std::fs::read(path)?;
    Ok(bincode::deserialize::<BoundWitnessCache>(&bytes)?.into_bound())
}

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
    // `td.block_id` is the raw SHA-256 envelope-tree root (big-endian byte
    // string). Both Circuit 1A/1B and Circuit 2 emit the block id as the
    // integer value of that BE digest, i.e. `bytes_le_to_fr(reverse(root))`
    // (see the synthetic builder in `layer_hashes_test_data.rs`, which mirrors
    // the circuit and reverses before `bytes_le_to_fr`). Interpreting the raw
    // BE bytes as little-endian (no reverse) yields a byte-reversed scalar that
    // fails the circuit's `block_id` instance equality. Reverse here so the
    // single shared `block_id_fr` matches what both circuits reconstruct.
    let block_id_fr = {
        let mut le = block_id_bytes;
        le.reverse();
        bytes_le_to_fr(&le)
    };

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
    // Off-by-one bridge between partner-generator and circuit semantics.
    //
    // The partner's `generate_layer_hash_chain(num_layers, num_prev_chain_steps)`
    // builds `num_prev_chain_steps + 1` ACTIVE trees (the trailing `+1` is the
    // current block's tree, whose root *is* `root_hashes[num_layers-1]`) but
    // records only the count of *previous* steps in
    // `LayerHashChainData.num_prev_chain_steps`.
    //
    // The circuit, however, treats this value as `num_active_steps` and inside
    // `verify_chain_of_dense_proofs` marks links `0..num_active_steps` active
    // (`active = is_less_than(j, num_active_steps)`), folding exactly that many
    // Merkle roots before comparing the result to `layer_hash_frs[num_layers-1]`.
    // Its own unit test (`build_test_chain(num_steps)` → pass `num_steps`) and
    // the synthetic builder both put the TOTAL active count in this slot.
    //
    // So we must hand the circuit the total active count (`+1`); copying the
    // partner's "previous" count verbatim folds one tree too few and the chain
    // result never reaches the target layer hash, producing a witness that
    // satisfies `MockProver`-free `create_proof` but fails verification under
    // every transcript. `num_prev_chain_steps + 1 <= MAX_CHAIN_LEN` is already
    // guaranteed by the generator, so the circuit's `[1, MAX_CHAIN_LEN]` range
    // check still holds.
    let num_prev_chain_steps = (td.layer_hash_chain.num_prev_chain_steps + 1) as u8;

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

    // ---- Re-sign the Primary attestation over the LE block_id ----
    // The generator-built `td.attestation_bytes` store the raw big-endian root,
    // which makes Circuit 1A emit a byte-reversed block_id (see
    // `build_attestation_envelope`). Re-sign over the LE root with the original
    // signers so Circuit 1A's block_id matches Circuit 2 + the on-chain anchor.
    let attestation_primary_bytes =
        build_attestation_envelope(&td, AttestationTargetType::Primary)?;

    // ---- Optional fallback attestation (same LE block_id) ----
    let attestation_fallback_bytes = if with_fallback {
        Some(build_attestation_envelope(&td, AttestationTargetType::Fallback)?)
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
        attestation_primary_bytes,
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

/// Re-sign an attestation envelope over the **little-endian** block_id with the
/// given target type, reusing `td.keypairs` (the original signers; the
/// partner's generator appends one extra keypair after the BK set was modified,
/// which we must skip here so we sign with exactly the *current* set).
///
/// Why re-sign and reverse the block_id? Circuit 1A/1B extract the block_id as
/// `LE-pack(raw attestation bytes)` (no reverse — see `primary_circuit.rs`
/// `build_primary_constraints` §A), whereas Circuit 2 reconstructs the SHA-256
/// envelope root and reverses BE→LE before packing
/// (`circuit.rs` §C: `LE-pack(reverse(root))`), which is also the value the
/// on-chain `uint256(sha256_root)` Merkle binding in `applyBkSetUpdate`
/// produces. The partner's generator stores the *raw* big-endian root in the
/// attestation, so a generator-built Circuit 1A proof emits a byte-reversed
/// block_id that can never equal Circuit 2's — `verifyBlock` feeds a single
/// `blockId` to both verifiers and requires equality. We therefore store the
/// block_id little-endian here so all three circuits emit the canonical
/// big-endian-digest integer. `td.block_id` keeps the raw root so Circuit 2's
/// sibling reconstruction and the shared `block_id_fr` stay consistent.
fn build_attestation_envelope(
    td: &BridgeTestData,
    target_type: AttestationTargetType,
) -> anyhow::Result<Vec<u8>> {
    let mut block_id_le = td.block_id;
    block_id_le.reverse();

    // td.keypairs has one extra entry at the end (the new signer for the BK
    // change scenario). Use only the original `bk_set.len()` signers.
    let original_signer_count = td.bk_set.len();
    let signers: Vec<(SignerIndex, &Secret)> = td
        .keypairs
        .iter()
        .take(original_signer_count)
        .map(|(secret, _, idx)| (*idx, secret))
        .collect();

    let attestation_data = create_attestation_data(block_id_le, target_type);
    let envelope = sign_attestation_multi(attestation_data, &signers)
        .context("signing attestation failed")?;
    let bytes = bincode::serialize(&envelope).context("bincoding attestation envelope failed")?;
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
