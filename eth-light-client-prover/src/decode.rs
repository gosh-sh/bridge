//! M4-fusion brick #1 — **compressed-G1 decode-bind**.
//!
//! The rotate proof commits (and SSZ-anchors) the committee as 48-byte
//! **compressed** pubkeys; the step proof aggregates `G1Affine` **points**. For
//! the Poseidon commitment to soundly stand in for the anchored committee, the
//! step must prove that each byte-string it committed decodes to the exact point
//! it aggregates — otherwise a prover could commit to bytes `X` but aggregate a
//! different point `Y` (e.g. `-pk_i`), forging the aggregate.
//!
//! [`assert_pubkey_bytes_bind_point`] enforces `bytes == compress(point)` for the
//! ZCash/IETF BLS12-381 compressed format used by Ethereum:
//!
//! - byte 0 top 3 bits are flags `C‖I‖S`: **C=1** (compressed, asserted),
//!   **I=0** (not infinity, asserted for a real pubkey), **S** = sort/sign bit.
//! - the remaining 381 bits are the big-endian x-coordinate.
//! - **S = 1 ⟺ y is the lexicographically larger root ⟺ y > (p−1)/2** (NOT the
//!   RFC-9380 parity `sgn0`), enforced by a limb-wise magnitude compare.
//!
//! x is bound by rebuilding the field element's 5 little-endian 104-bit limbs
//! directly from the bytes (104 bits = exactly 13 bytes, so the byte↔limb split
//! is clean) and constraining them equal to `point.x`'s limbs. y is bound via the
//! sign bit. Together this pins the full point.

use crate::bls_core::{LIMB_BITS, NUM_LIMBS};
use halo2_base::gates::{GateInstructions, RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bls12_381::Fq;
use halo2_base::utils::{decompose_biguint, modulus, BigPrimeField};
use halo2_base::{AssignedValue, Context, QuantumCell};
use halo2_ecc::bigint::ProperCrtUint;
use halo2_ecc::ecc::EcPoint;
use num_bigint::BigUint;
use QuantumCell::{Constant, Existing};

/// An assigned BLS12-381 G1 point over the [`halo2_ecc::bls12_381::FpChip`].
pub type AssignedG1<F> = EcPoint<F, ProperCrtUint<F>>;

/// `(p−1)/2` for the BLS12-381 base field, as `NUM_LIMBS` little-endian limbs.
fn half_modulus_limbs<F: BigPrimeField>() -> Vec<F> {
    let p = modulus::<Fq>();
    let half: BigUint = (&p - BigUint::from(1u64)) / BigUint::from(2u64);
    decompose_biguint::<F>(&half, NUM_LIMBS, LIMB_BITS)
}

/// Rebuild one little-endian 104-bit limb from big-endian bytes: the limb's most
/// significant byte is `eb[hi]`, ... least significant is `eb[hi - (n-1)]`.
fn recon_limb<F: BigPrimeField>(
    gate: &impl GateInstructions<F>,
    ctx: &mut Context<F>,
    eb: &[AssignedValue<F>],
    hi: usize,
    n: usize,
) -> AssignedValue<F> {
    let mut acc = ctx.load_zero();
    let mut base = F::ONE;
    for m in 0..n {
        let term = gate.mul(ctx, eb[hi - m], Constant(base));
        acc = gate.add(ctx, acc, term);
        base *= F::from(256u64);
    }
    acc
}

/// Return a boolean cell = `a > b` (lexicographic, MSL-first) for equal-length
/// limb vectors, `a` assigned and `b` constant.
fn limbs_gt_const<F: BigPrimeField>(
    range: &RangeChip<F>,
    ctx: &mut Context<F>,
    a_limbs: &[AssignedValue<F>],
    b_limbs: &[F],
) -> AssignedValue<F> {
    let gate = range.gate();
    let mut gt = ctx.load_zero();
    let mut eq = ctx.load_constant(F::ONE);
    for j in (0..a_limbs.len()).rev() {
        let a = a_limbs[j];
        let b = b_limbs[j];
        // a > b  ⟺  b < a
        let a_gt = range.is_less_than(ctx, Constant(b), Existing(a), LIMB_BITS);
        let this_gt = gate.and(ctx, eq, a_gt);
        gt = gate.or(ctx, gt, this_gt);
        let a_eq = gate.is_equal(ctx, a, Constant(b));
        eq = gate.and(ctx, eq, a_eq);
    }
    gt
}

/// Constrain that the 48 compressed `bytes` decode to `point` (see module docs).
/// Panics on the wrong byte count; constraint violations surface in the prover.
pub fn assert_pubkey_bytes_bind_point<F: BigPrimeField>(
    range: &RangeChip<F>,
    ctx: &mut Context<F>,
    bytes: &[AssignedValue<F>],
    point: &AssignedG1<F>,
) {
    assert_eq!(bytes.len(), 48, "compressed G1 pubkey must be 48 bytes");
    let gate = range.gate();

    // Range-check the 47 low bytes; byte 0 is bounded by the div_mod below.
    for b in &bytes[1..] {
        range.range_check(ctx, *b, 8);
    }

    // byte0 = 32·flags3 + x_hi5, flags3 = 4·C + 2·I + S.
    let (flags3, x_hi5) = range.div_mod(ctx, bytes[0], BigUint::from(32u64), 8);
    let flag_bits = gate.num_to_bits(ctx, flags3, 3); // [S, I, C] little-endian
    let s = flag_bits[0];
    let one = ctx.load_constant(F::ONE);
    let zero = ctx.load_zero();
    ctx.constrain_equal(&flag_bits[2], &one); // C == 1 (compressed)
    ctx.constrain_equal(&flag_bits[1], &zero); // I == 0 (not infinity)

    // Effective big-endian x bytes: eb[0] = masked top byte, eb[k] = bytes[k].
    let mut eb: Vec<AssignedValue<F>> = Vec::with_capacity(48);
    eb.push(x_hi5);
    eb.extend_from_slice(&bytes[1..]);

    // Rebuild x's 5 LE 104-bit limbs and pin them to point.x.
    // limb0←BE[35..47], limb1←BE[22..34], limb2←BE[9..21], limb3←BE[0..8], limb4=0.
    let x_limbs = point.x().limbs();
    let limb0 = recon_limb(gate, ctx, &eb, 47, 13);
    let limb1 = recon_limb(gate, ctx, &eb, 34, 13);
    let limb2 = recon_limb(gate, ctx, &eb, 21, 13);
    let limb3 = recon_limb(gate, ctx, &eb, 8, 9);
    ctx.constrain_equal(&limb0, &x_limbs[0]);
    ctx.constrain_equal(&limb1, &x_limbs[1]);
    ctx.constrain_equal(&limb2, &x_limbs[2]);
    ctx.constrain_equal(&limb3, &x_limbs[3]);
    ctx.constrain_equal(&zero, &x_limbs[4]);

    // Sign bit: S == (y > (p−1)/2).
    let half = half_modulus_limbs::<F>();
    let y_gt = limbs_gt_const(range, ctx, point.y().limbs(), &half);
    ctx.constrain_equal(&s, &y_gt);
}
