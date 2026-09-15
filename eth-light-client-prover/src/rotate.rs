//! M3-rotate — committee ↔ anchor binding.
//!
//! The step circuit ([`crate::step`]) aggregates 512 sync-committee pubkeys that
//! are, on their own, unconstrained witnesses: a malicious prover could pick any
//! committee that signs. This module produces the **anchor** that ties a
//! committee to the beacon chain, plus the cheap **Poseidon commitment** the step
//! circuit will recompute so the two proofs chain.
//!
//! ## What a rotate proof establishes
//!
//! 1. `sync_committee_root = htr(SyncCommittee)` over the 512 pubkeys + aggregate
//!    (the expensive ~1023-SHA256 SSZ merkleization — [`crate::committee`]).
//! 2. `next_sync_committee_branch` @ [`NEXT_SYNC_COMMITTEE_GINDEX`] reconstructs a
//!    trusted beacon **`state_root`** (the same state the step's attested header
//!    commits to). Post-Electra gindex is **87** (validated on live mainnet data).
//! 3. A **Poseidon commitment** `commit_sync_committee(pubkeys, aggregate)` over
//!    the identical 48-byte pubkey witnesses.
//!
//! The rotate proof runs once per sync-committee period (~27 h), so the k≈26 SSZ
//! cost is amortized. Its cheap output — the Poseidon commitment — is what the
//! frequent step proof recomputes.
//!
//! ## Fusion seam (M4)
//!
//! Full soundness closes when the step circuit (a) is handed the same 48-byte
//! **compressed** pubkeys, (b) **decode-binds** them to the `G1Affine` points it
//! aggregates (compressed-G1 decode gadget: BE x + sign/inf flag + on-curve — the
//! `deserialize_pubkey` constraint noted in `committee.rs`), (c) recomputes
//! [`commit_sync_committee`] over those bytes, and (d) asserts it equals the
//! rotate proof's committed value via the public instance. Until (b) lands,
//! wiring the commitment into step would be soundness theater (bytes committed ≠
//! points aggregated), so this module ships the anchor + commitment standalone.

use crate::committee::{committee_subtree_root, sync_committee_root, SYNC_COMMITTEE_SIZE};
use crate::ssz::{verify_merkle_branch, Node};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::GateInstructions;
use halo2_base::poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher};
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, Context, QuantumCell};

/// Electra/Fulu generalized index of `next_sync_committee` in `BeaconState`.
/// `floor(log2(87)) = 6` → 6-element branch. (Pre-Electra was 55; validated 87
/// against live mainnet update data — see `tests/rotate_mock_prover.rs`.)
pub const NEXT_SYNC_COMMITTEE_GINDEX: u64 = 87;

/// Electra/Fulu generalized index of `current_sync_committee` in `BeaconState`
/// (used by the weak-subjectivity bootstrap). Depth 6.
pub const CURRENT_SYNC_COMMITTEE_GINDEX: u64 = 86;

// Standard BN254 Poseidon parameters (T=3, RATE=2, R_F=8, R_P=57) — the same
// spec halo2-lib's own hasher tests use, so the native `pse_poseidon` twin and
// the in-circuit `hash_fix_len_array` agree.
const POSEIDON_T: usize = 3;
const POSEIDON_RATE: usize = 2;
const POSEIDON_R_F: usize = 8;
const POSEIDON_R_P: usize = 57;

/// Construct + initialize the committee Poseidon hasher. Call once per circuit.
pub fn new_committee_hasher<F: BigPrimeField>(
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
) -> PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE> {
    let spec = OptimizedPoseidonSpec::<F, POSEIDON_T, POSEIDON_RATE>::new::<
        POSEIDON_R_F,
        POSEIDON_R_P,
        0,
    >();
    let mut hasher = PoseidonHasher::<F, POSEIDON_T, POSEIDON_RATE>::new(spec);
    hasher.initialize_consts(ctx, gate);
    hasher
}

/// Little-endian byte-slice → single field element (`Σ b_i · 256^i`).
fn le_field<F: BigPrimeField>(
    ctx: &mut Context<F>,
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

/// Pack a committee into field elements: each 48-byte compressed pubkey → two
/// limbs (`[0..31]`, `[31..48]`; the 31-byte low limb is < 2^248 < |F|), then the
/// aggregate pubkey likewise. `512·2 + 2 = 1026` elements.
fn pack_committee<F: BigPrimeField>(
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkeys: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
) -> Vec<AssignedValue<F>> {
    assert_eq!(pubkeys.len(), SYNC_COMMITTEE_SIZE * 48, "expected 512*48 pubkey bytes");
    assert_eq!(aggregate.len(), 48, "aggregate_pubkey must be 48 bytes");
    let mut elems = Vec::with_capacity(SYNC_COMMITTEE_SIZE * 2 + 2);
    for pk in pubkeys.chunks(48) {
        elems.push(le_field(ctx, gate, &pk[0..31]));
        elems.push(le_field(ctx, gate, &pk[31..48]));
    }
    elems.push(le_field(ctx, gate, &aggregate[0..31]));
    elems.push(le_field(ctx, gate, &aggregate[31..48]));
    elems
}

/// Poseidon commitment to a sync committee (cheap; ~513 permutations). Both the
/// rotate proof and the step proof recompute this over the *same* 48-byte
/// witnesses so the frequent step proof inherits the rotate anchor.
///
/// **Single-sponge (v1).** Superseded by the 2-level scheme
/// ([`commit_sync_committee_2level`]) once the recursive rotate (M6) lands, because
/// the aggregation circuit does not hold all 512 pubkeys and so cannot recompute a
/// single sponge over them. Kept intact until the coordinated step+rotate VkBlob
/// re-emit so the current step Vk stays valid.
pub fn commit_sync_committee<F: BigPrimeField>(
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkeys: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
) -> AssignedValue<F> {
    let elems = pack_committee(ctx, gate, pubkeys, aggregate);
    hasher.hash_fix_len_array(ctx, gate, &elems)
}

// ---------------------------------------------------------------------------
// 2-level committee commitment (M6 recursive rotate)
// ---------------------------------------------------------------------------
//
// The single sponge above needs all 512 pubkeys in one circuit. Under the
// recursive rotate the pubkeys live in N shard proofs, so the commitment is
// computed in two levels:
//
//   shard_digest_j       = Poseidon( pack_pubkeys(slice_j) )          // per shard
//   committee_commitment = Poseidon( [shard_digest_0..N, agg_lo, agg_hi] )
//
// Each shard binds its digest to the *same* pubkey witnesses it merkleizes for the
// SHA subtree; the aggregation combines the shard digests + aggregate. `step` (which
// holds all 512) computes the identical value via `commit_sync_committee_2level`, so
// the on-chain `_currentCommittee` matches whichever side produced it. See
// `docs/m6_rotate_recursive.md`.

/// Pack a pubkey slice (no aggregate) into 2 limbs each: `n*48` bytes → `2n` limbs.
fn pack_pubkeys<F: BigPrimeField>(
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkeys: &[AssignedValue<F>],
) -> Vec<AssignedValue<F>> {
    assert!(pubkeys.len() % 48 == 0, "pubkey bytes must be a multiple of 48");
    let mut elems = Vec::with_capacity(pubkeys.len() / 48 * 2);
    for pk in pubkeys.chunks(48) {
        elems.push(le_field(ctx, gate, &pk[0..31]));
        elems.push(le_field(ctx, gate, &pk[31..48]));
    }
    elems
}

/// Pack the aggregate pubkey into its 2 limbs `[lo(0..31), hi(31..48)]`.
fn pack_aggregate<F: BigPrimeField>(
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    aggregate: &[AssignedValue<F>],
) -> [AssignedValue<F>; 2] {
    assert_eq!(aggregate.len(), 48, "aggregate_pubkey must be 48 bytes");
    [le_field(ctx, gate, &aggregate[0..31]), le_field(ctx, gate, &aggregate[31..48])]
}

/// Level 1: Poseidon digest of one committee shard (its pubkeys only). A shard
/// proof exposes this as a public output; `step` recomputes it per shard.
pub fn commit_committee_shard<F: BigPrimeField>(
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkey_slice: &[AssignedValue<F>],
) -> AssignedValue<F> {
    let elems = pack_pubkeys(ctx, gate, pubkey_slice);
    hasher.hash_fix_len_array(ctx, gate, &elems)
}

/// Level 2: combine N shard digests + aggregate into the committee commitment. Run
/// by the aggregation circuit (over snark-verified shard digests) and by `step`
/// (over locally-computed digests) — they must produce the identical value.
pub fn combine_committee_commitment<F: BigPrimeField>(
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    shard_digests: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
) -> AssignedValue<F> {
    let mut elems = shard_digests.to_vec();
    let agg = pack_aggregate(ctx, gate, aggregate);
    elems.push(agg[0]);
    elems.push(agg[1]);
    hasher.hash_fix_len_array(ctx, gate, &elems)
}

/// Full 2-level committee commitment for a caller holding all 512 pubkeys (`step`).
/// Equals `combine(commit_committee_shard(slice_j) for each shard, aggregate)`.
/// `pubkeys_per_shard` must divide the committee into a whole number of shards.
pub fn commit_sync_committee_2level<F: BigPrimeField>(
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkeys: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
    pubkeys_per_shard: usize,
) -> AssignedValue<F> {
    assert!(pubkeys.len() % 48 == 0, "pubkey bytes must be a multiple of 48");
    let n = pubkeys.len() / 48;
    assert!(
        pubkeys_per_shard > 0 && n % pubkeys_per_shard == 0,
        "pubkeys_per_shard must divide the committee"
    );
    let digests: Vec<AssignedValue<F>> = pubkeys
        .chunks(pubkeys_per_shard * 48)
        .map(|slice| commit_committee_shard(hasher, ctx, gate, slice))
        .collect();
    combine_committee_commitment(hasher, ctx, gate, &digests, aggregate)
}

// ---------------------------------------------------------------------------
// Shard inner circuit (M6 recursive rotate)
// ---------------------------------------------------------------------------

/// Public-instance layout of a committee **shard** proof (length 3), all BN254 Fr:
/// ```text
/// [0] subtree_root_hi   = LE(subtree_root[0..16])
/// [1] subtree_root_lo   = LE(subtree_root[16..32])
/// [2] shard_digest      = Poseidon commitment over this shard's pubkeys
/// ```
/// The aggregation circuit snark-verifies N of these and reads the instances back
/// (re-merkleize the subtree roots into the committee SHA root; combine the
/// shard digests into the committee Poseidon commitment). Hi/lo split matches
/// `step`'s `node_hi_lo` convention so the two sides agree byte-for-byte.
pub const SHARD_INSTANCE_LEN: usize = 3;

/// Build one shard proof's public instances from a contiguous pubkey slice: the
/// SHA subtree root (hi/lo) + the Poseidon shard digest, both bound to the SAME
/// byte-witnesses. This is the inner circuit the recursive aggregation consumes.
pub fn verify_shard<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkey_slice: &[AssignedValue<F>],
) -> Vec<AssignedValue<F>> {
    let subtree_root = committee_subtree_root(chip, ctx, pubkey_slice);
    let digest = commit_committee_shard(hasher, ctx, gate, pubkey_slice);
    let hi = le_field(ctx, gate, &subtree_root[0..16]);
    let lo = le_field(ctx, gate, &subtree_root[16..32]);
    vec![hi, lo, digest]
}

/// The full rotate constraint: bind the committee's SSZ root to `state_root` via
/// `branch` @ `gindex`, and return the Poseidon commitment over the same pubkeys.
///
/// Panics on structurally invalid input (wrong committee size / branch length);
/// constraint failures surface in the prover.
#[allow(clippy::too_many_arguments)]
pub fn verify_rotate<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    hasher: &PoseidonHasher<F, POSEIDON_T, POSEIDON_RATE>,
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    pubkeys: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
    branch: &[Node<F>],
    gindex: u64,
    state_root: &Node<F>,
) -> AssignedValue<F> {
    let scr = sync_committee_root(chip, ctx, pubkeys, aggregate);
    verify_merkle_branch(chip, ctx, &scr, branch, gindex, state_root);
    commit_sync_committee(hasher, ctx, gate, pubkeys, aggregate)
}
