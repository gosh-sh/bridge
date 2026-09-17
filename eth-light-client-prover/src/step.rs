//! M3 — the light-client **"step"** circuit core.
//!
//! Proves, entirely in-circuit, that one sync-committee update is valid:
//!
//! 1. **signing root** (M2): SSZ `hash_tree_root(attested.beacon)` → `domain`
//!    (fork_version @ signature_slot + `genesis_validators_root`) → `signing_root`.
//! 2. **hash-to-curve** (M1): RFC-9380 `signing_root` → G2 message point (`POP_` DST).
//! 3. **committee aggregate**: `agg = Σ bit_i · pk_i` over the 512 committee pubkeys,
//!    with the 2/3 supermajority (`3·Σbit ≥ 2·512 = 1024`, i.e. ≥ 342) enforced.
//! 4. **signature validity**: the G2 signature is on-curve **and subgroup-checked**
//!    (M-hardening; the *same* assigned point feeds the pairing — sound binding),
//!    then `e(-G1, sig) · e(agg, H(signing_root)) == 1`.
//! 5. **finality**: `hash_tree_root(finalized.beacon)` + `finality_branch` at the
//!    Electra/Fulu gindex reconstruct `attested.beacon.state_root` (M2 branch).
//!
//! 6. **committee decode-bind** (M4-fusion): each aggregated pubkey is bound to
//!    its 48-byte compressed encoding, and the committee **Poseidon commitment**
//!    (= the rotate proof's output) is recomputed — so the aggregated committee
//!    is the chain-anchored one, not a prover-chosen set.
//! 7. **execution block_hash** (M4-fusion): `htr(ExecutionPayloadHeader)` +
//!    `execution_branch` reconstruct the *same* `finalized.body_root`, exposing
//!    the finalized execution block hash (the deposit-path output).
//!
//! Outputs [`StepPublicInputs`] (attested/finalized slot, finalized beacon root,
//! participation, committee commitment, execution block hash) and packs them into
//! the canonical [`StepPublicInputs::instances`] layout (see [`pack_step_instances`])
//! ready to be committed to the circuit's single instance column — the public-input
//! shape the `ZKHALO2VERIFYWITHVK` VkBlob will pin.

use crate::bls_core::{ETH_BLS_DST, LIMB_BITS, NUM_LIMBS, SYNC_COMMITTEE_SIZE};
use crate::decode::assert_pubkey_bytes_bind_point;
use crate::execution::{execution_payload_root, ExecutionPayloadVals, EXECUTION_PAYLOAD_GINDEX};
use crate::committee::PUBKEYS_PER_SHARD;
use crate::rotate::{commit_sync_committee_2level, new_committee_hasher};
use crate::signing::{beacon_header_root, compute_domain, signing_root as ssz_signing_root};
use crate::ssz::{load_bytes, load_node, verify_merkle_branch, Node};
use crate::subgroup::assert_g2_in_subgroup;
use gosh_bls_verification::{compute_all_pub_sum, load_bk_set_pubkeys};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::flex_gate::threads::SinglePhaseCoreManager;
use halo2_base::gates::{GateInstructions, RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1Affine, G2Affine};
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, QuantumCell};
use halo2_ecc::bls12_381::bls_signature::BlsSignatureChip;
use halo2_ecc::bls12_381::pairing::PairingChip;
use halo2_ecc::bls12_381::{Fp2Chip, FpChip, G2Point};
use halo2_ecc::ecc::hash_to_curve::{ExpandMsgXmd, HashToCurveChip};
use halo2_ecc::ecc::{check_is_on_curve, EccChip};

/// Electra/Fulu generalized index of `finalized_checkpoint.root` in `BeaconState`
/// (M0 spec §3). `floor(log2(169)) = 7` → 7-element branch.
pub const FINALIZED_ROOT_GINDEX: u64 = 169;

/// A `BeaconBlockHeader`'s five fields as native values.
#[derive(Clone)]
pub struct HeaderVals {
    pub slot: u64,
    pub proposer_index: u64,
    pub parent_root: [u8; 32],
    pub state_root: [u8; 32],
    pub body_root: [u8; 32],
}

/// All native witness data for one light-client step.
pub struct StepWitness {
    pub attested: HeaderVals,
    pub finalized: HeaderVals,
    pub finality_branch: Vec<[u8; 32]>,
    pub fork_version: [u8; 4],
    pub genesis_validators_root: [u8; 32],
    pub pubkeys: Vec<G1Affine>,
    /// 48-byte **compressed** encoding of each `pubkeys[i]` (decode-bound in-circuit).
    pub pubkeys_compressed: Vec<[u8; 48]>,
    /// `SyncCommittee.aggregate_pubkey` bytes (committed alongside the pubkeys).
    pub aggregate_pubkey: [u8; 48],
    pub bits: Vec<bool>,
    pub signature: G2Affine,
    /// Finalized block's execution payload header (its `block_hash` is exposed).
    pub finalized_execution: ExecutionPayloadVals,
    /// `execution_branch` proving `htr(execution)` against `finalized.body_root`.
    pub execution_branch: Vec<[u8; 32]>,
}

/// Number of field elements in the step circuit's public-instance column.
///
/// Layout (see [`pack_step_instances`]):
/// `[attested_slot, finalized_slot, finalized_beacon_root_hi, finalized_beacon_root_lo,
///   participation, committee_commitment, execution_block_hash_hi, execution_block_hash_lo,
///   attested_state_root_hi, attested_state_root_lo]`.
///
/// Indices 8/9 expose the **attested beacon `state_root`** — the state the current
/// committee's BLS signature commits to (it is hashed into the signing root and is
/// the finality-branch anchor). The recursive rotate aggregation binds its
/// `next_sync_committee_branch` anchor to this value (R2 soundness, `bls-state-root`),
/// so the successor committee is proven to sit in the *signed* state.
pub const STEP_INSTANCE_LEN: usize = 10;

/// The step circuit's public outputs.
///
/// The individual assigned pieces are kept for callers that want them typed; the
/// flattened [`Self::instances`] vector is the canonical order committed to the
/// single public-instance column (length [`STEP_INSTANCE_LEN`]).
pub struct StepPublicInputs<F: BigPrimeField> {
    pub attested_slot: AssignedValue<F>,
    pub finalized_slot: AssignedValue<F>,
    pub finalized_beacon_root: Node<F>,
    pub participation: AssignedValue<F>,
    /// **2-level** Poseidon commitment to the committee (shard digests → combine) —
    /// byte-identical to the recursive rotate's `next_commit`, so the on-chain
    /// `_currentCommittee` a rotate advances into matches what this step recomputes.
    pub committee_commitment: AssignedValue<F>,
    /// The finalized **execution** block hash (the deposit-path output).
    pub execution_block_hash: Node<F>,
    /// The **attested beacon `state_root`** — bound by the BLS signature (part of
    /// the signed header) and the finality-branch anchor. Exposed so the rotate
    /// aggregation can anchor `next_sync_committee_branch` to the *signed* state.
    pub attested_state_root: Node<F>,
    /// Canonical ordered public inputs, ready for the instance column.
    pub instances: Vec<AssignedValue<F>>,
}

/// Recompose little-endian value bytes into a single field element.
fn le_to_field<F: BigPrimeField>(
    ctx: &mut halo2_base::Context<F>,
    gate: &impl GateInstructions<F>,
    bytes: &[AssignedValue<F>],
) -> AssignedValue<F> {
    let mut acc = ctx.load_constant(F::ZERO);
    let mut base = F::ONE;
    for &b in bytes {
        let term = gate.mul(ctx, b, QuantumCell::Constant(base));
        acc = gate.add(ctx, acc, term);
        base *= F::from(256u64);
    }
    acc
}

/// Split a 32-byte SSZ root into two field elements: `hi = LE(root[0..16])`,
/// `lo = LE(root[16..32])`. Each half is < 2^128, so the pair is injective and
/// fits the BN254 scalar field. This is how a 32-byte root is exposed to the
/// (BN254-scalar) instance column and reassembled by the AN-side consumer.
fn node_hi_lo<F: BigPrimeField>(
    ctx: &mut halo2_base::Context<F>,
    gate: &impl GateInstructions<F>,
    node: &Node<F>,
) -> (AssignedValue<F>, AssignedValue<F>) {
    assert_eq!(node.len(), 32, "SSZ root must be 32 bytes");
    (le_to_field(ctx, gate, &node[0..16]), le_to_field(ctx, gate, &node[16..32]))
}

/// Pack the step's public outputs into the canonical instance-column order
/// (length [`STEP_INSTANCE_LEN`]):
///
/// ```text
/// [0] attested_slot                 [1] finalized_slot
/// [2] finalized_beacon_root_hi      [3] finalized_beacon_root_lo
/// [4] participation                 [5] committee_commitment
/// [6] execution_block_hash_hi       [7] execution_block_hash_lo
/// [8] attested_state_root_hi        [9] attested_state_root_lo
/// ```
///
/// 32-byte roots are split hi/lo via [`node_hi_lo`]; slots / participation are
/// already single field elements, and the committee commitment is a Poseidon
/// output field element.
#[allow(clippy::too_many_arguments)]
pub fn pack_step_instances<F: BigPrimeField>(
    ctx: &mut halo2_base::Context<F>,
    gate: &impl GateInstructions<F>,
    attested_slot: AssignedValue<F>,
    finalized_slot: AssignedValue<F>,
    finalized_beacon_root: &Node<F>,
    participation: AssignedValue<F>,
    committee_commitment: AssignedValue<F>,
    execution_block_hash: &Node<F>,
    attested_state_root: &Node<F>,
) -> Vec<AssignedValue<F>> {
    let (fbr_hi, fbr_lo) = node_hi_lo(ctx, gate, finalized_beacon_root);
    let (bh_hi, bh_lo) = node_hi_lo(ctx, gate, execution_block_hash);
    let (sr_hi, sr_lo) = node_hi_lo(ctx, gate, attested_state_root);
    vec![
        attested_slot,
        finalized_slot,
        fbr_hi,
        fbr_lo,
        participation,
        committee_commitment,
        bh_hi,
        bh_lo,
        sr_hi,
        sr_lo,
    ]
}

fn load_header<F: BigPrimeField>(
    ctx: &mut halo2_base::Context<F>,
    h: &HeaderVals,
) -> (Vec<AssignedValue<F>>, Vec<AssignedValue<F>>, Node<F>, Node<F>, Node<F>) {
    (
        load_bytes(ctx, &h.slot.to_le_bytes()),
        load_bytes(ctx, &h.proposer_index.to_le_bytes()),
        load_node(ctx, &h.parent_root),
        load_node(ctx, &h.state_root),
        load_node(ctx, &h.body_root),
    )
}

/// Assemble and constrain the full step. Panics on structurally invalid witness
/// (wrong committee size / branch length); constraint failures surface in the prover.
pub fn verify_step<F: BigPrimeField>(
    pool: &mut SinglePhaseCoreManager<F>,
    range: &RangeChip<F>,
    sha: &Sha256Chip<F>,
    w: &StepWitness,
) -> StepPublicInputs<F> {
    assert_eq!(w.pubkeys.len(), SYNC_COMMITTEE_SIZE, "expected 512 committee pubkeys");
    assert_eq!(
        w.pubkeys_compressed.len(),
        SYNC_COMMITTEE_SIZE,
        "expected 512 compressed committee pubkeys"
    );
    assert_eq!(w.bits.len(), SYNC_COMMITTEE_SIZE, "expected 512 participation bits");
    assert_eq!(w.finality_branch.len(), 7, "finality_branch must be depth 7 (gindex 169)");
    assert_eq!(w.execution_branch.len(), 4, "execution_branch must be depth 4 (gindex 25)");

    let fp_chip = FpChip::<F>::new(range, LIMB_BITS, NUM_LIMBS);
    let fp2_chip = Fp2Chip::<F>::new(&fp_chip);
    let g1_chip = EccChip::new(&fp_chip);
    let g2_chip = EccChip::new(&fp2_chip);
    let pairing_chip = PairingChip::new(&fp_chip);
    let bls_chip = BlsSignatureChip::new(&fp_chip, &pairing_chip);
    let gate = range.gate();

    // ---------------------------------------------------------------------
    // Phase A (single-threaded ctx): SSZ signing root, finality branch,
    // committee load, bitmask + threshold, shifted MSM scalars.
    // ---------------------------------------------------------------------
    let (
        signing_node,
        finalized_beacon_root,
        attested_state_root,
        attested_slot,
        finalized_slot,
        assigned_pks,
        all_pub_sum,
        shifted_scalars,
        participation,
        f_body,
    ) = {
        let ctx = pool.main();

        let (a_slot, a_prop, a_parent, a_state, a_body) = load_header(ctx, &w.attested);
        let (f_slot, f_prop, f_parent, f_state, f_body) = load_header(ctx, &w.finalized);
        let fork = load_bytes(ctx, &w.fork_version);
        let gvr = load_node(ctx, &w.genesis_validators_root);

        let attested_slot = le_to_field(ctx, gate, &a_slot);
        let finalized_slot = le_to_field(ctx, gate, &f_slot);

        // signing_root = SHA256( htr(attested.beacon) ‖ domain )
        let attested_object_root =
            beacon_header_root(sha, ctx, &a_slot, &a_prop, &a_parent, &a_state, &a_body);
        let domain = compute_domain(sha, ctx, &fork, &gvr);
        let signing_node = ssz_signing_root(sha, ctx, &attested_object_root, &domain);

        // finality: htr(finalized.beacon) + finality_branch reconstructs attested state_root.
        let finalized_beacon_root =
            beacon_header_root(sha, ctx, &f_slot, &f_prop, &f_parent, &f_state, &f_body);
        let branch: Vec<Node<F>> = w.finality_branch.iter().map(|b| load_node(ctx, b)).collect();
        verify_merkle_branch(sha, ctx, &finalized_beacon_root, &branch, FINALIZED_ROOT_GINDEX, &a_state);

        // committee pubkeys (on-curve) + sum for the shifted-MSM correction.
        let assigned_pks = load_bk_set_pubkeys(ctx, range, &w.pubkeys, LIMB_BITS, NUM_LIMBS);
        let all_pub_sum = compute_all_pub_sum(ctx, range, &assigned_pks, LIMB_BITS, NUM_LIMBS);

        // bitmask: boolean, count participation, build shifted scalars (bit + 1).
        let one = ctx.load_constant(F::ONE);
        let mut participation = ctx.load_constant(F::ZERO);
        let mut shifted_scalars: Vec<Vec<AssignedValue<F>>> = Vec::with_capacity(SYNC_COMMITTEE_SIZE);
        for &b in &w.bits {
            let bit = ctx.load_witness(F::from(b as u64));
            gate.assert_bit(ctx, bit);
            participation = gate.add(ctx, participation, bit);
            let shifted = gate.add(ctx, bit, one);
            shifted_scalars.push(vec![shifted]);
        }

        // supermajority: 3·participation - 2·512 >= 0  (i.e. participation >= 342).
        {
            let three = ctx.load_constant(F::from(3u64));
            let three_p = gate.mul(ctx, three, participation);
            let bound = ctx.load_constant(F::from(2 * SYNC_COMMITTEE_SIZE as u64));
            let slack = gate.sub(ctx, three_p, bound);
            range.range_check(ctx, slack, 16);
        }

        (
            signing_node,
            finalized_beacon_root,
            a_state,
            attested_slot,
            finalized_slot,
            assigned_pks,
            all_pub_sum,
            shifted_scalars,
            participation,
            f_body,
        )
    };

    // ---------------------------------------------------------------------
    // Phase A2 (ctx): M4-fusion — bind each aggregated pubkey to its committed
    // compressed bytes, recompute the committee Poseidon commitment, and bind
    // the finalized execution block hash. Uses the *same* `f_body` cells the
    // finality path proved, so the execution root hangs off the finalized block.
    // ---------------------------------------------------------------------
    let (committee_commitment, execution_block_hash) = {
        let ctx = pool.main();
        let gate = range.gate();

        let flat: Vec<u8> =
            w.pubkeys_compressed.iter().flat_map(|p| p.iter().copied()).collect();
        let pk_bytes = load_bytes(ctx, &flat);
        let agg_bytes = load_bytes(ctx, &w.aggregate_pubkey);

        // decode-bind: each committed 48-byte pubkey == compress(aggregated point).
        for (i, chunk) in pk_bytes.chunks(48).enumerate() {
            assert_pubkey_bytes_bind_point(range, ctx, chunk, &assigned_pks[i]);
        }

        // committee Poseidon commitment — **2-level** (M6): shard digests over
        // PUBKEYS_PER_SHARD-pubkey slices, then combine with the aggregate. This is
        // the identical value the recursive rotate emits as `next_commit`
        // (`combine_committee_commitment` over snark-verified shard digests), so the
        // on-chain `_currentCommittee` a rotate advances into matches what the next
        // step's `submitUpdate` recomputes.
        let hasher = new_committee_hasher(ctx, gate);
        let committee_commitment =
            commit_sync_committee_2level(&hasher, ctx, gate, &pk_bytes, &agg_bytes, PUBKEYS_PER_SHARD);

        // execution block_hash: htr(ExecutionPayloadHeader) + execution_branch @ 25
        // reconstruct the finalized body_root (same cells as the finality path).
        let (exec_root, block_hash) = execution_payload_root(sha, ctx, &w.finalized_execution);
        let exec_branch: Vec<Node<F>> =
            w.execution_branch.iter().map(|b| load_node(ctx, b)).collect();
        verify_merkle_branch(sha, ctx, &exec_root, &exec_branch, EXECUTION_PAYLOAD_GINDEX, &f_body);

        (committee_commitment, block_hash)
    };

    // ---------------------------------------------------------------------
    // Phase B (pool): in-circuit hash-to-curve of the signing root to G2.
    // ---------------------------------------------------------------------
    let msghash: G2Point<F> = {
        let h2c = HashToCurveChip::new(sha, &fp2_chip);
        h2c.hash_to_curve::<ExpandMsgXmd>(
            pool,
            signing_node.iter().map(|c| QuantumCell::Existing(*c)),
            ETH_BLS_DST,
        )
        .expect("in-circuit hash-to-curve failed")
    };

    // ---------------------------------------------------------------------
    // Phase C (pool): MSM  Σ pk_i·(bit_i+1) = agg + all_pub_sum.
    // ---------------------------------------------------------------------
    let msm = g1_chip.variable_base_msm::<G1Affine>(pool, &assigned_pks, shifted_scalars, 2);

    // ---------------------------------------------------------------------
    // Phase D (ctx): recover agg, load+validate signature, pairing check.
    // ---------------------------------------------------------------------
    let ctx = pool.main();
    let neg_all_pub_sum = g1_chip.negate(ctx, all_pub_sum);
    let agg = g1_chip.add_unequal(ctx, msm, neg_all_pub_sum, true);

    let sig_pt = pairing_chip.load_private_g2_unchecked(ctx, w.signature);
    check_is_on_curve::<F, Fp2Chip<F>, G2Affine>(&fp2_chip, ctx, &sig_pt);
    assert_g2_in_subgroup(&g2_chip, ctx, &sig_pt);
    bls_chip.assert_valid_signature(ctx, sig_pt, msghash, agg);

    // ---------------------------------------------------------------------
    // Public inputs: pack into the canonical instance-column layout. The caller
    // assigns this vector to the circuit's single instance column.
    // ---------------------------------------------------------------------
    let instances = pack_step_instances(
        ctx,
        gate,
        attested_slot,
        finalized_slot,
        &finalized_beacon_root,
        participation,
        committee_commitment,
        &execution_block_hash,
        &attested_state_root,
    );
    debug_assert_eq!(instances.len(), STEP_INSTANCE_LEN);

    StepPublicInputs {
        attested_slot,
        finalized_slot,
        finalized_beacon_root,
        participation,
        committee_commitment,
        execution_block_hash,
        attested_state_root,
        instances,
    }
}
