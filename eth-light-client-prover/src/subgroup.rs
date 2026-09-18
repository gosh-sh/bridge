//! In-circuit BLS12-381 **G2 subgroup membership check** — closes audit finding
//! BLS-1 / FORK-2 ("no G2 subgroup check; G2 cofactor ≠ 1").
//!
//! The sync-committee signature is a single G2 point. `check_is_on_curve` alone is
//! not enough: an on-curve point may live outside the prime-order subgroup, which
//! can make the pairing equation hold for a forged signature. We enforce subgroup
//! membership with the endomorphism trick (Scott, eprint 2021/1130, proof
//! 2022/352): `P ∈ 𝔾₂  ⟺  ψ(P) = [x]P`, where `x = -0xd201000000010000` is the
//! BLS parameter. halo2curves' native `is_torsion_free` computes exactly
//! `psi(P) == mul_by_x(P)` with `mul_by_x(P) = [x]P = -[BLS_X]P`. halo2-ecc's
//! `mul_by_bls_x` returns `[BLS_X]P` (positive magnitude), so in-circuit we check
//! **`ψ(P) == -[BLS_X]P`**.
//!
//! Cost: one 64-bit `mul_by_bls_x` (scalar mult over the seed) + one `ψ` (two Fp2
//! conjugations + two Fp2 muls) — far cheaper than the naive `[r]P == O`
//! (255-bit) check.
//!
//! **Soundness note (M3 binding):** the subgroup check must constrain the *same*
//! assigned G2 cells that feed the pairing. Use [`load_checked_g2`] to load the
//! signature once (on-curve + subgroup) and pass the returned point into the
//! pairing. Do **not** run the check on a separately-loaded copy of the signature.

use crate::bls_core::{LIMB_BITS, NUM_LIMBS};
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bls12_381::G2Affine;
use halo2_base::utils::BigPrimeField;
use halo2_base::Context;
use halo2_ecc::bls12_381::pairing::PairingChip;
use halo2_ecc::bls12_381::{Fp2Chip, FpChip, G2Point};
use halo2_ecc::ecc::hash_to_curve::HashToCurveInstructions;
use halo2_ecc::ecc::{check_is_on_curve, EccChip};

/// Enforce `P ∈ 𝔾₂` via `ψ(P) == -[BLS_X]P`. Assumes `P` is already on-curve.
pub fn assert_g2_in_subgroup<'a, F: BigPrimeField>(
    g2_chip: &EccChip<'a, F, Fp2Chip<'a, F>>,
    ctx: &mut Context<F>,
    p: &G2Point<F>,
) {
    let x_p = g2_chip.mul_by_bls_x(ctx, p.clone()); // [BLS_X]P
    let neg_x_p = g2_chip.negate(ctx, x_p); // -[BLS_X]P = [x]P
    let psi_p = g2_chip.psi(ctx, p.clone()); // ψ(P)
    g2_chip.assert_equal(ctx, psi_p, neg_x_p);
}

/// Load a G2 signature once and fully validate it (on-curve **and** subgroup),
/// returning the assigned point to be reused in the pairing. This is the
/// soundness-preserving entry point for the light-client "step" circuit.
pub fn load_checked_g2<F: BigPrimeField>(
    range: &RangeChip<F>,
    ctx: &mut Context<F>,
    signature: G2Affine,
) -> G2Point<F> {
    let fp_chip = FpChip::<F>::new(range, LIMB_BITS, NUM_LIMBS);
    let fp2_chip = Fp2Chip::<F>::new(&fp_chip);
    let g2_chip = EccChip::new(&fp2_chip);
    let pairing_chip = PairingChip::new(&fp_chip);

    let p = pairing_chip.load_private_g2_unchecked(ctx, signature);
    check_is_on_curve::<F, Fp2Chip<F>, G2Affine>(&fp2_chip, ctx, &p);
    assert_g2_in_subgroup(&g2_chip, ctx, &p);
    p
}
