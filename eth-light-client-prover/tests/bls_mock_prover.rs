//! M1 MockProver tests for the BLS core.
//!
//! All tests here are `#[ignore]` because each spins a k>=18 halo2 circuit over
//! BLS12-381 non-native arithmetic (minutes, GBs of RAM) — run on n14:
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test bls_mock_prover -- --ignored --nocapture
//! ```
//!
//! Coverage:
//! - `hash_to_curve_matches_native`  — in-circuit RFC-9380 h2c == native (DST wiring)
//! - `small_committee_aggregate`     — gosh aggregate gadget, 4 signers (fast wiring check)
//! - `full_512_synthetic`            — `verify_sync_aggregate` end-to-end at real width 512

use std::marker::PhantomData;

use eth_light_client_prover::bls_core::{
    assign_signing_root_hash_to_g2, signing_root_to_g2, verify_sync_aggregate, LIMB_BITS, NUM_LIMBS,
    SYNC_COMMITTEE_SIZE,
};
use gosh_bls_verification::{
    compute_all_pub_sum, load_bk_set_pubkeys, verify_bls_attestation_with_assigned_msghash,
    ThresholdMode,
};
use halo2_base::gates::flex_gate::threads::SinglePhaseCoreManager;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1Affine, G2Affine, G1, G2};
use halo2_base::halo2_proofs::halo2curves::CurveAffine;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::halo2curves::group::{Curve, Group};
use halo2_base::halo2_proofs::plonk::Error;
use halo2_base::utils::testing::base_test;
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, QuantumCell};
use halo2_ecc::bls12_381::pairing::PairingChip;
use halo2_ecc::bls12_381::{Fp2Chip, FpChip};
use halo2_ecc::ecc::hash_to_curve::HashInstructions;
use halo2_ecc::fields::FieldChip;
use rand::rngs::OsRng;

/// Off-circuit SHA-256 (structural placeholder for M1). M2 replaces this with the
/// in-circuit `gosh-sha256-chip` so the digest itself is constrained. Copied from
/// halo2-ecc's `bls12_381::tests::hash_to_curve`.
#[derive(Clone, Copy, Debug, Default)]
struct Sha256MockChip<F>(PhantomData<F>);

impl<F: BigPrimeField> HashInstructions<F> for Sha256MockChip<F> {
    const BLOCK_SIZE: usize = 64;
    const DIGEST_SIZE: usize = 32;

    type CircuitBuilder = SinglePhaseCoreManager<F>;
    type Output = Vec<AssignedValue<F>>;

    fn digest(
        &self,
        thread_pool: &mut Self::CircuitBuilder,
        input: impl IntoIterator<Item = QuantumCell<F>>,
    ) -> Result<Vec<AssignedValue<F>>, Error> {
        use sha2::{Digest, Sha256};
        let input_bytes = input
            .into_iter()
            .map(|b| match b {
                QuantumCell::Witness(b) => b.get_lower_32() as u8,
                QuantumCell::Constant(b) => b.get_lower_32() as u8,
                QuantumCell::Existing(av) => av.value().get_lower_32() as u8,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let output = Sha256::digest(&input_bytes)
            .into_iter()
            .map(|b| thread_pool.main().load_witness(F::from(b as u64)))
            .collect();
        Ok(output)
    }

    fn digest_varlen(
        &self,
        _ctx: &mut Self::CircuitBuilder,
        _input: impl IntoIterator<Item = QuantumCell<F>>,
        _max_input_len: usize,
    ) -> Result<Self::Output, Error> {
        unimplemented!()
    }
}

/// Build `n` signers all participating: returns (pubkeys, signature over `msg_hash`).
fn synthetic_committee(n: usize, msg_hash: G2Affine) -> (Vec<G1Affine>, G2Affine) {
    let hm = G2::from(msg_hash);
    let mut pubkeys = Vec::with_capacity(n);
    let mut sig = G2::identity();
    for _ in 0..n {
        let sk = <G1 as Group>::Scalar::random(OsRng);
        pubkeys.push((G1::generator() * sk).to_affine());
        sig += hm * sk;
    }
    (pubkeys, sig.to_affine())
}

#[test]
#[ignore = "k>=18 BLS12-381 circuit; run on n14"]
fn hash_to_curve_matches_native() {
    let signing_root = [42u8; 32];
    let native = signing_root_to_g2(&signing_root);

    base_test().k(18).lookup_bits(17).run_builder(|pool, range| {
        let sha = Sha256MockChip::<_>::default();
        let assigned = assign_signing_root_hash_to_g2(pool, range, &sha, &signing_root);

        let fp_chip = FpChip::<_>::new(range, LIMB_BITS, NUM_LIMBS);
        let fp2_chip = Fp2Chip::new(&fp_chip);
        let got = G2Affine::from_xy(
            fp2_chip.get_assigned_value(&assigned.x.into()),
            fp2_chip.get_assigned_value(&assigned.y.into()),
        )
        .unwrap();
        assert_eq!(got, native, "in-circuit hash-to-curve != native (DST/impl drift)");
    });
}

#[test]
#[ignore = "k>=20 BLS12-381 circuit; run on n14"]
fn small_committee_aggregate() {
    // 4-of-4 signers exercises MSM + Primary threshold + pairing cheaply.
    const N: usize = 4;
    let signing_root = [1u8; 32];
    let msg_hash = signing_root_to_g2(&signing_root);
    let (pubkeys, signature) = synthetic_committee(N, msg_hash);
    let signers_data: Vec<(u16, u16)> = (0..N as u16).map(|i| (i, 1)).collect();

    base_test().k(20).lookup_bits(19).run_builder(|pool, range| {
        let fp_chip = FpChip::<_>::new(range, LIMB_BITS, NUM_LIMBS);
        let pairing_chip = PairingChip::new(&fp_chip);
        let (assigned_pks, all_pub_sum, actual_npk, msghash_assigned) = {
            let ctx = pool.main();
            let msghash_assigned = pairing_chip.load_private_g2_unchecked(ctx, msg_hash);
            let assigned_pks = load_bk_set_pubkeys(ctx, range, &pubkeys, LIMB_BITS, NUM_LIMBS);
            let all_pub_sum = compute_all_pub_sum(ctx, range, &assigned_pks, LIMB_BITS, NUM_LIMBS);
            let actual_npk = ctx.load_witness(Fr::from(N as u64));
            (assigned_pks, all_pub_sum, actual_npk, msghash_assigned)
        };
        verify_bls_attestation_with_assigned_msghash(
            pool,
            range,
            signature,
            msghash_assigned,
            &assigned_pks,
            &signers_data,
            N,
            LIMB_BITS,
            NUM_LIMBS,
            ThresholdMode::Primary,
            all_pub_sum,
            actual_npk,
        );
    });
}

#[test]
#[ignore = "k>=21 BLS12-381 circuit over 512 signers; run on n14 (tune k)"]
fn full_512_synthetic() {
    // Full-width path through the M1 public entrypoint: 512 committee members,
    // all participating (participation 512 >= 342), in-circuit hash-to-curve.
    let signing_root = [3u8; 32];
    let msg_hash = signing_root_to_g2(&signing_root);
    let (pubkeys, signature) = synthetic_committee(SYNC_COMMITTEE_SIZE, msg_hash);
    let bits = vec![true; SYNC_COMMITTEE_SIZE];

    base_test().k(22).lookup_bits(21).run_builder(|pool, range| {
        let sha = Sha256MockChip::<_>::default();
        let msghash_assigned = assign_signing_root_hash_to_g2(pool, range, &sha, &signing_root);
        verify_sync_aggregate(pool, range, &pubkeys, &bits, signature, msghash_assigned);
    });
}
