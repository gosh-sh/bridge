//! G2 subgroup-check tests (closes BLS-1 / FORK-2).
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test subgroup_mock_prover -- --ignored --nocapture
//! ```
//!
//! - positive: a genuine 𝔾₂ point (`G2Affine::random`, cofactor-cleared) passes;
//! - negative: an on-curve point *outside* the subgroup makes the check fail.
//!
//! The non-subgroup point is built the way halo2curves' own `G2::random` does,
//! but **without** `clear_cofactor`: sample `x`, solve `y² = x³ + B` (B recovered
//! from the real generator so we never hardcode it), load via
//! `from_uncompressed_unchecked_be`, and keep it only if `!is_torsion_free`.

use eth_light_client_prover::subgroup::load_checked_g2;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{Fq2, G2Affine};
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::utils::testing::base_test;
use halo2_base::Context;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use rand::rngs::OsRng;

/// Build an on-curve 𝔾₂ point that is NOT in the prime-order subgroup.
fn non_subgroup_g2() -> G2Affine {
    // Recover the curve B = y² - x³ from the (on-curve, subgroup) generator so we
    // never depend on the private `B` constant.
    let g = G2Affine::generator();
    let b: Fq2 = g.y.square() - (g.x.square() * g.x);

    for _ in 0..4000 {
        let x = Fq2::random(OsRng);
        let rhs = x.square() * x + b;
        let y = Option::<Fq2>::from(rhs.sqrt());
        let y = match y {
            Some(y) => y,
            None => continue,
        };
        // Uncompressed big-endian layout: x.c1 ‖ x.c0 ‖ y.c1 ‖ y.c0 (flag bits 0).
        let mut bytes = [0u8; 192];
        bytes[0..48].copy_from_slice(&x.c1.to_bytes_be());
        bytes[48..96].copy_from_slice(&x.c0.to_bytes_be());
        bytes[96..144].copy_from_slice(&y.c1.to_bytes_be());
        bytes[144..192].copy_from_slice(&y.c0.to_bytes_be());
        let p = Option::<G2Affine>::from(G2Affine::from_uncompressed_unchecked_be(&bytes));
        if let Some(p) = p {
            if bool::from(p.is_on_curve()) && !bool::from(p.is_torsion_free()) {
                return p;
            }
        }
    }
    panic!("failed to sample a non-subgroup G2 point");
}

#[test]
fn native_non_subgroup_point_is_off_subgroup() {
    let p = non_subgroup_g2();
    assert!(bool::from(p.is_on_curve()), "must be on curve");
    assert!(!bool::from(p.is_torsion_free()), "must be outside subgroup");
    // sanity: a random cofactor-cleared point IS torsion-free
    let good = G2Affine::random(OsRng);
    assert!(bool::from(good.is_torsion_free()));
}

#[test]
#[ignore = "in-circuit G2 ops; run on n14"]
fn subgroup_check_accepts_valid_point() {
    let good = G2Affine::random(OsRng);
    assert!(bool::from(good.is_torsion_free()));
    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let _p = load_checked_g2(range, ctx, good);
    });
}

#[test]
#[ignore = "in-circuit G2 ops; run on n14"]
#[should_panic]
fn subgroup_check_rejects_non_subgroup_point() {
    let bad = non_subgroup_g2();
    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let _p = load_checked_g2(range, ctx, bad);
    });
}
