//! Bound-direction-bit variant of `gosh_dense_balanced_tree::dense_merkle_root_circuit_padded`.
//!
//! # Why this module exists
//!
//! The upstream `dense_merkle_root_circuit_padded` walker loads a fresh
//! `assert_bit`-only witness per level for its left/right direction bit
//! (`level.direction_bit`). That is soundness-neutral **only** when the
//! caller's position isn't otherwise committed to — the prover has full
//! freedom to pick any orientation that satisfies the root equation.
//!
//! `bridge-event-prove-circuit` folds `events_pos` into the nullifier
//! Poseidon preimage (BRIDGE-WD-01) so that two identical
//! `WithdrawalInitiated` events in the same AN block produce distinct
//! nullifiers. That defense is only sound if the `events_pos` fed into the
//! nullifier hash is the same `events_pos` that determines the events-tree
//! merkle path — otherwise a malicious prover picks any position to
//! disambiguate the hash and walks the tree along a different path.
//! Binding requires the walker's direction bits to come from the
//! bit-decomposition of the assigned `events_pos` witness.
//!
//! [`dense_merkle_root_padded_bound`] is byte-for-byte identical to the
//! upstream walker except the direction bit at level `j` is
//! `pos_bits[j]` (caller-supplied) instead of a fresh witness. The caller
//! is expected to derive `pos_bits` from the position witness via
//! `gate.num_to_bits` (which combines range check + bit decomposition),
//! and to enforce `pos_bits[j] == 0` for `j >= num_active_levels` so the
//! bit convention matches `preprocess_dense_proof_padded`'s
//! `direction_bit = false` on padded levels.
//!
//! Ported verbatim from `dexdo-halo2-kit/dex-halo2-circuit/src/dense_merkle_bound.rs`.
//! A future PR should lift this into `gosh-dense-balanced-tree` so both
//! circuits share one implementation.

use gosh_dense_balanced_tree::{bytes_to_fr, cond_swap, DenseTreeProof};
use halo2_base::gates::{GateInstructions, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::poseidon::hasher::PoseidonHasher;
use halo2_base::{AssignedValue, Context};

use crate::poseidon::{RATE, T};

/// In-circuit dense-merkle walk with direction bits bound to an external
/// position witness.
///
/// Mirrors upstream `gosh_dense_balanced_tree::dense_merkle_root_circuit_padded`
/// verbatim except for one change: at each level `j`, the direction bit is
/// **`pos_bits[j]`** — the caller-supplied bit-decomposition of the position
/// witness — instead of a fresh unconstrained witness loaded from
/// `level.direction_bit` and merely `assert_bit`-checked. Semantics are
/// identical when the caller passes bits that match the preprocessed
/// `direction_bit`s.
///
/// Preconditions the caller MUST enforce:
/// * `pos_bits.len() == proof.levels.len()`.
/// * Each `pos_bits[j] ∈ {0, 1}` (satisfied automatically when produced by
///   `gate.num_to_bits`).
/// * `sum(pos_bits[j] · 2^j) == pos_witness` for whichever `pos_witness` the
///   caller is binding (satisfied by `gate.num_to_bits`).
/// * For every `j ≥ num_active_levels`, `pos_bits[j] == 0`. This aligns the
///   bound direction bit with the preprocessor's `direction_bit = false`
///   convention on padded levels, so the chunk-decomposition constraints
///   inside the walk hold uniformly across active/inactive levels.
pub fn dense_merkle_root_padded_bound(
    ctx: &mut Context<Fr>,
    range: &impl RangeInstructions<Fr>,
    hasher: &PoseidonHasher<Fr, T, RATE>,
    proof: &DenseTreeProof,
    leaf_fr: AssignedValue<Fr>,
    num_active_levels: AssignedValue<Fr>,
    pos_bits: &[AssignedValue<Fr>],
) -> AssignedValue<Fr> {
    assert_eq!(
        pos_bits.len(),
        proof.levels.len(),
        "pos_bits length must match proof.levels",
    );
    let gate = range.gate();

    let pow_248 = ctx.load_constant(Fr::from_raw([0u64, 0u64, 0u64, 1u64 << 56]));
    let pow_240 = ctx.load_constant(Fr::from_raw([0u64, 0u64, 0u64, 1u64 << 48]));
    let two56 = ctx.load_constant(Fr::from(256u64));

    let mut cur = leaf_fr;

    for (j, level) in proof.levels.iter().enumerate() {
        // active = (j < num_active_levels)
        let j_const = ctx.load_constant(Fr::from(j as u64));
        let active = range.is_less_than(ctx, j_const, num_active_levels, 4);

        let sibling_fr = ctx.load_witness(bytes_to_fr(&level.sibling));

        // Direction bit is the bound `pos_bits[j]`, NOT a free witness.
        // The caller enforces `pos_bits[j] == 0` for `j >= num_active_levels`,
        // so on padded levels this matches the preprocessor's
        // `direction_bit = false` convention.
        let bit = pos_bits[j];

        let (left, right) = cond_swap(ctx, gate, cur, sibling_fr, bit);

        let c0 = ctx.load_witness(level.chunk0);
        let c1 = ctx.load_witness(level.chunk1);
        let c2 = ctx.load_witness(level.chunk2);
        let left_hi = ctx.load_witness(Fr::from(level.left_hi as u64));

        let lhs = gate.mul_add(ctx, left_hi, pow_248, c0);
        ctx.constrain_equal(&lhs, &left);

        let c2_shifted = gate.mul(ctx, c2, pow_240);
        let right_low = gate.sub(ctx, right, c2_shifted);

        let rhs = gate.mul_add(ctx, right_low, two56, left_hi);
        ctx.constrain_equal(&rhs, &c1);

        range.range_check(ctx, c0, 248);
        range.range_check(ctx, right_low, 240);
        range.range_check(ctx, left_hi, 8);
        range.range_check(ctx, c2, 16);

        let computed = hasher.hash_fix_len_array(ctx, gate, &[c0, c1, c2]);

        cur = gate.select(ctx, computed, cur, active);
    }

    cur
}

// ---------------------------------------------------------------------------
// BRIDGE-WD-01 gadget-level tests
// ---------------------------------------------------------------------------
//
// These tests exercise `dense_merkle_root_padded_bound` in isolation — no
// SHA-256, no BOC parsing, no dense-chain walk — so a failure points
// directly at the position-binding gadget rather than any of the surrounding
// pipeline. They cover:
//
//   * positive walks at bit patterns today's fixtures never touch
//     (pos = 255 at max depth; pos = 129 on a 130-leaf non-power-of-two
//     tree — same depth-8 tree the events proof walks on chain);
//   * a flipped bit inside the active range diverges the computed root;
//   * the gadget ALONE cannot reject a bit set above `num_active_levels`
//     (this is the BRIDGE-WD-01 gap — `gate.select` discards the walked
//     path on padded levels, so the extra bit is invisible to the walker
//     but would still fold into any nullifier hash the caller wires the
//     same position witness into);
//   * the caller-level zero-forcing loop used in
//     `bridge_event_prove_circuit::synthesize` closes that gap by pinning
//     `pos_bits[j] == 0` for every padded level.
#[cfg(test)]
mod tests {
    use super::*;
    use dense_balanced_tree::{
        dense_merkle_proof, dense_merkle_root, PoseidonHasher as DensePoseidonHasher,
    };
    use gosh_dense_balanced_tree::{bytes_to_fr, preprocess_dense_proof_padded};
    use halo2_base::gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams};
    use halo2_base::gates::{GateInstructions, RangeChip, RangeInstructions};
    use halo2_base::halo2_proofs::dev::MockProver;
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
    use halo2_base::poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher};
    use halo2_base::{AssignedValue, QuantumCell};

    use crate::poseidon::{RATE, R_F, R_P, T};

    // Small circuit — no SHA-256, no dense-chain — so K=11 with `lookup_bits=10`
    // is more than enough for the ~30 range checks the gadget emits.
    const K: u32 = 11;
    const LOOKUP_BITS: usize = 10;
    const MAX_DEPTH: usize = 8;

    fn base_params() -> BaseCircuitParams {
        BaseCircuitParams {
            k: K as usize,
            num_advice_per_phase: vec![4],
            num_fixed: 1,
            num_lookup_advice_per_phase: vec![1],
            lookup_bits: Some(LOOKUP_BITS),
            num_instance_columns: 0,
        }
    }

    /// Little-endian bit decomposition of `pos` into exactly `depth` bits.
    fn bit_vec(pos: u64, depth: usize) -> Vec<u64> {
        (0..depth).map(|j| (pos >> j) & 1).collect()
    }

    /// Overwrite `proof.levels[j]` with sibling/chunk witnesses consistent
    /// with `bit = 1` (attacker-chosen orientation) at that level, given the
    /// `cur_bytes` value that `dense_merkle_root_padded_bound` will see on
    /// entry to level `j`. This is the exact malicious-prover freedom the
    /// caller-level zero-forcing loop closes: on padded (inactive) levels
    /// `gate.select` discards the walker's computed hash, so a bit set above
    /// `num_active_levels` is invisible to the walker as long as the chunk
    /// decomposition matches the flipped orientation — which the prover
    /// controls entirely via `load_witness`.
    ///
    /// Concretely, for `bit = 1` the walker's `cond_swap` gives
    /// `(left, right) = (sibling, cur)`, so we set:
    ///   chunk0  = Fr(sibling[0..31] LE)
    ///   chunk1  = Fr(sibling[31] || cur[0..30] LE)
    ///   chunk2  = Fr(cur[30..32] LE)
    ///   left_hi = sibling[31]
    /// which satisfies every algebraic linking constraint inside the walker.
    fn tamper_level_to_bit_1(
        proof: &mut gosh_dense_balanced_tree::DenseTreeProof,
        j: usize,
        cur_bytes: [u8; 32],
    ) {
        // Arbitrary sibling — the constraint system doesn't tie `sibling` to
        // anything else on padded levels. Zero is the simplest choice.
        let sibling = [0u8; 32];
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&sibling);
        concat[32..].copy_from_slice(&cur_bytes);
        let mut buf0 = [0u8; 32];
        buf0[..31].copy_from_slice(&concat[0..31]);
        let mut buf1 = [0u8; 32];
        buf1[..31].copy_from_slice(&concat[31..62]);
        let mut buf2 = [0u8; 32];
        buf2[..2].copy_from_slice(&concat[62..64]);

        proof.levels[j].sibling = sibling;
        proof.levels[j].chunk0 = bytes_to_fr(&buf0);
        proof.levels[j].chunk1 = bytes_to_fr(&buf1);
        proof.levels[j].chunk2 = bytes_to_fr(&buf2);
        proof.levels[j].left_hi = sibling[31];
        // `direction_bit` isn't consulted by `dense_merkle_root_padded_bound` —
        // the bound walker takes `pos_bits[j]` from the caller instead.
    }

    /// Build a random `num_leaves`-leaf tree (the library pads to the next
    /// power of two internally) and return `(leaf@pos, siblings, root)`.
    fn build_tree(
        num_leaves: usize,
        pos: usize,
        seed: u64,
    ) -> ([u8; 32], Vec<[u8; 32]>, [u8; 32]) {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let mut leaves = vec![[0u8; 32]; num_leaves];
        let mut rng = StdRng::seed_from_u64(seed);
        for leaf in leaves.iter_mut() {
            rng.fill(leaf);
        }
        let hasher = DensePoseidonHasher::new();
        let siblings = dense_merkle_proof(&hasher, &leaves, pos);
        let root = dense_merkle_root(&hasher, &leaves);
        (leaves[pos], siblings, root)
    }

    /// Instantiate a tiny circuit that:
    ///   1. loads `leaf`, `num_active`, and `pos_bits_vals` as witnesses;
    ///   2. optionally runs the same zero-forcing loop
    ///      `bridge_event_prove_circuit::synthesize` runs before calling
    ///      the walker (BRIDGE-WD-01);
    ///   3. calls `dense_merkle_root_padded_bound`;
    ///   4. constrains the result to equal `expected_root`.
    ///
    /// Returns `true` iff `MockProver::verify()` accepts.
    fn run_gadget(
        proof: &gosh_dense_balanced_tree::DenseTreeProof,
        leaf_bytes: [u8; 32],
        num_active: u64,
        pos_bits_vals: &[u64],
        expected_root: [u8; 32],
        with_zero_forcing: bool,
    ) -> bool {
        assert_eq!(pos_bits_vals.len(), proof.levels.len());

        let mut builder = BaseCircuitBuilder::<Fr>::new(false).use_params(base_params());
        let range =
            RangeChip::<Fr>::new(LOOKUP_BITS, builder.lookup_manager().clone());

        {
            let ctx = builder.main(0);

            let spec = OptimizedPoseidonSpec::<Fr, T, RATE>::new::<R_F, R_P, 0>();
            let mut hasher = PoseidonHasher::<Fr, T, RATE>::new(spec);
            hasher.initialize_consts(ctx, range.gate());

            let leaf_fr = ctx.load_witness(bytes_to_fr(&leaf_bytes));

            let num_active_fr = ctx.load_witness(Fr::from(num_active));
            range.range_check(ctx, num_active_fr, 4);

            let pos_bits: Vec<AssignedValue<Fr>> = pos_bits_vals
                .iter()
                .map(|b| {
                    let v = ctx.load_witness(Fr::from(*b));
                    range.gate().assert_bit(ctx, v);
                    v
                })
                .collect();

            if with_zero_forcing {
                for (j, bit) in pos_bits.iter().enumerate() {
                    let j_const = ctx.load_constant(Fr::from(j as u64));
                    let active_j =
                        range.is_less_than(ctx, j_const, num_active_fr, 4);
                    let one_const = ctx.load_constant(Fr::one());
                    let inactive_j = range.gate().sub(
                        ctx,
                        QuantumCell::Existing(one_const),
                        QuantumCell::Existing(active_j),
                    );
                    let prod = range.gate().mul(
                        ctx,
                        QuantumCell::Existing(*bit),
                        QuantumCell::Existing(inactive_j),
                    );
                    range.gate().assert_is_const(ctx, &prod, &Fr::zero());
                }
            }

            let root_fr = dense_merkle_root_padded_bound(
                ctx,
                &range,
                &hasher,
                proof,
                leaf_fr,
                num_active_fr,
                &pos_bits,
            );

            let expected_fr = ctx.load_witness(bytes_to_fr(&expected_root));
            ctx.constrain_equal(&root_fr, &expected_fr);
        }

        builder.calculate_params(Some(20));
        let prover = MockProver::run(K, &builder, vec![]).unwrap();
        prover.verify().is_ok()
    }

    /// Positive: top of the active range (`pos = 255`, all 8 direction bits
    /// set) on a full depth-8 tree. Today's fixtures only walk pos 0/3/7,
    /// so no existing test exercises the high half of the bit space.
    #[test]
    fn test_dense_merkle_bound_positive_pos_255_depth_8() {
        let (leaf, siblings, root) = build_tree(1 << MAX_DEPTH, 255, 0xF00D);
        assert_eq!(siblings.len(), MAX_DEPTH);
        let proof = preprocess_dense_proof_padded(leaf, &siblings, 255, MAX_DEPTH);
        assert!(
            run_gadget(&proof, leaf, MAX_DEPTH as u64, &bit_vec(255, MAX_DEPTH), root, true),
            "pos=255 walk over depth-8 tree must satisfy",
        );
    }

    /// Positive: `pos = 129` on a 130-leaf events tree — the library pads to
    /// the next power of two (depth = 8), so this is the last "real" leaf
    /// and its bit pattern (`0b10000001`) has both endpoints set, unlike the
    /// small positions today's fixtures use.
    #[test]
    fn test_dense_merkle_bound_positive_pos_129_non_pow2() {
        let (leaf, siblings, root) = build_tree(130, 129, 0x1234);
        assert_eq!(siblings.len(), MAX_DEPTH);
        let proof = preprocess_dense_proof_padded(leaf, &siblings, 129, MAX_DEPTH);
        assert!(
            run_gadget(&proof, leaf, MAX_DEPTH as u64, &bit_vec(129, MAX_DEPTH), root, true),
            "pos=129 walk over 130-leaf (depth-8) tree must satisfy",
        );
    }

    /// Negative: a bit flipped inside the active range makes the walker take
    /// a different path — the computed root diverges from the honest one,
    /// so the equality constraint on `expected_root` fails.
    #[test]
    fn test_dense_merkle_bound_negative_flipped_bit_within_active_levels() {
        let (leaf, siblings, root) = build_tree(1 << 3, 5, 0xBEEF);
        let proof = preprocess_dense_proof_padded(leaf, &siblings, 5, MAX_DEPTH);
        let mut bits = bit_vec(5, MAX_DEPTH);
        bits[1] ^= 1; // b101 -> b111
        assert!(
            !run_gadget(&proof, leaf, 3, &bits, root, true),
            "flipping pos_bits[1] within active levels must desynchronize \
             the walked path from `expected_root`",
        );
    }

    /// Bug demonstration: `dense_merkle_root_padded_bound` ALONE has no way
    /// to reject `pos_bits[j] == 1` for `j >= num_active_levels` when the
    /// prover is willing to tamper the padded level's witnesses. On padded
    /// levels `gate.select` discards the walker's computed hash, so `cur`
    /// stays at the honest root regardless of the direction bit; the only
    /// per-level constraints (chunk decomposition + range checks) are on
    /// the prover-supplied `sibling`/`chunk0..2`/`left_hi`, all of which the
    /// attacker chooses via `load_witness`. Choosing them consistently with
    /// `bit = 1` (via [`tamper_level_to_bit_1`]) satisfies every constraint,
    /// so the walker accepts a `pos_witness = p + 2^d`.
    ///
    /// This is exactly the BRIDGE-WD-01 gap: absent the caller-level
    /// zero-forcing loop, a malicious prover would witness
    /// `events_pos = p + 2^d`, walk the correct path in the events tree,
    /// and hash a DIFFERENT position into the nullifier — collapsing two
    /// distinct events into one nullifier slot and permanently blocking
    /// one on-chain payout.
    #[test]
    fn test_dense_merkle_bound_gadget_alone_accepts_bit_above_active_levels() {
        let (leaf, siblings, root) = build_tree(1 << 3, 5, 0xCAFE);
        let mut proof = preprocess_dense_proof_padded(leaf, &siblings, 5, MAX_DEPTH);
        // Padded levels leave `cur` unchanged at the honest root, so at entry
        // to level 5 the walker's `cur` is `bytes_to_fr(&root)` — that's the
        // `cur_bytes` we need to sew the tampered chunk decomposition against.
        tamper_level_to_bit_1(&mut proof, 5, root);
        let mut bits = bit_vec(5, MAX_DEPTH);
        bits[5] = 1; // pos_witness = 5 + 2^5 = 37, bit 5 sits above active levels 0..3
        assert!(
            run_gadget(&proof, leaf, 3, &bits, root, /* with_zero_forcing */ false),
            "regression witness: the gadget alone must NOT reject a bit set \
             above `num_active_levels` when the prover supplies matching \
             tampered chunks — that job belongs to the caller-level \
             zero-forcing loop (see BRIDGE-WD-01 fix in \
             `bridge_event_prove_circuit::synthesize`)",
        );
    }

    /// Fix demonstration: same tampered proof + `pos_bits` as the previous
    /// test, but this time the caller wraps the walker with the BRIDGE-WD-01
    /// zero-forcing loop (`pos_bits[j] * (1 - active_j) == 0`). Bit 5 = 1
    /// on an inactive level trips `assert_is_const(prod, 0)`.
    #[test]
    fn test_dense_merkle_bound_zero_forcing_rejects_bit_above_active_levels() {
        let (leaf, siblings, root) = build_tree(1 << 3, 5, 0xCAFE);
        let mut proof = preprocess_dense_proof_padded(leaf, &siblings, 5, MAX_DEPTH);
        tamper_level_to_bit_1(&mut proof, 5, root);
        let mut bits = bit_vec(5, MAX_DEPTH);
        bits[5] = 1;
        assert!(
            !run_gadget(&proof, leaf, 3, &bits, root, /* with_zero_forcing */ true),
            "BRIDGE-WD-01: the caller-level zero-forcing loop must reject any \
             pos_bits[j]==1 for j >= num_active_levels — otherwise a prover \
             could hash `p + 2^d` into the nullifier while walking `p` in \
             the events tree",
        );
    }
}
