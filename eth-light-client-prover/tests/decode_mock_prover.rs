//! M4-fusion brick #1 tests: compressed-G1 decode-bind.
//!
//! ```bash
//! cd eth-light-client-prover
//! scripts/fetch_lc_fixtures.sh mainnet
//! cargo test --test decode_mock_prover                         # native convention check
//! cargo test --test decode_mock_prover -- --ignored --nocapture   # in-circuit (n14)
//! ```
//!
//! - `native_sign_flag_matches_compressed` (all 512 live pubkeys): the
//!   lexicographic sign convention `y > (p−1)/2` equals the ZCash compressed
//!   flag bit — validates the rule the circuit enforces.
//! - `decode_bind_accepts_real_pubkeys` (#[ignore]): the in-circuit gadget binds
//!   real compressed bytes to the decoded point.
//! - `decode_bind_rejects_tampered_x` / `_rejects_negated_y` (#[ignore]): the
//!   gadget rejects a mismatched x byte / a negated point (wrong sign).

use eth_light_client_prover::bls_core::{LIMB_BITS, NUM_LIMBS};
use eth_light_client_prover::decode::assert_pubkey_bytes_bind_point;
use eth_light_client_prover::ssz::load_bytes;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{Fq, G1Affine};
use halo2_base::utils::{modulus, BigPrimeField};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::utils::testing::base_test;
use halo2_base::Context;
use halo2_ecc::bls12_381::pairing::PairingChip;
use halo2_ecc::bls12_381::FpChip;
use halo2_ecc::ecc::check_is_on_curve;
use num_bigint::BigUint;
use serde_json::Value;
use std::path::PathBuf;

fn load_update() -> Option<Value> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/mainnet");
    let entry = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).find(|e| {
        let n = e.file_name();
        let n = n.to_string_lossy();
        n.starts_with("update_period_") && n.ends_with(".json")
    })?;
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
    Some(if raw.is_array() { raw[0]["data"].clone() } else { raw["data"].clone() })
}

fn pubkeys(v: &Value) -> Vec<[u8; 48]> {
    v["next_sync_committee"]["pubkeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| hex::decode(p.as_str().unwrap().trim_start_matches("0x")).unwrap().try_into().unwrap())
        .collect()
}

fn decode_point(bytes: &[u8; 48]) -> G1Affine {
    // ZCash/IETF big-endian compressed encoding (as served by the beacon API).
    Option::from(G1Affine::from_compressed_unchecked_be(bytes)).expect("valid compressed pubkey")
}

/// Native lexicographic sign flag: `y > (p-1)/2`.
fn native_sign_flag(p: &G1Affine) -> bool {
    let y = BigUint::from_bytes_le(&p.y.to_bytes());
    let half = (modulus::<Fq>() - BigUint::from(1u64)) / BigUint::from(2u64);
    y > half
}

fn load_point<F: BigPrimeField>(
    ctx: &mut Context<F>,
    range: &RangeChip<F>,
    p: G1Affine,
) -> eth_light_client_prover::decode::AssignedG1<F> {
    let fp_chip = FpChip::<F>::new(range, LIMB_BITS, NUM_LIMBS);
    let pairing_chip = PairingChip::new(&fp_chip);
    let pt = pairing_chip.load_private_g1_unchecked(ctx, p);
    check_is_on_curve::<F, FpChip<F>, G1Affine>(&fp_chip, ctx, &pt);
    pt
}

#[test]
fn native_sign_flag_matches_compressed() {
    let Some(v) = load_update() else {
        eprintln!("SKIP: no update fixture (run scripts/fetch_lc_fixtures.sh)");
        return;
    };
    let pks = pubkeys(&v);
    assert_eq!(pks.len(), 512);
    for (i, pk) in pks.iter().enumerate() {
        let pt = decode_point(pk);
        let compressed_flag = (pk[0] >> 5) & 1 == 1;
        assert_eq!(
            native_sign_flag(&pt),
            compressed_flag,
            "sign convention mismatch at pubkey {i}"
        );
        // sanity: also not-infinity, compressed bit set
        assert_eq!(pk[0] >> 7, 1, "C bit must be set");
        assert_eq!((pk[0] >> 6) & 1, 0, "I bit must be clear");
    }
}

#[test]
#[ignore = "in-circuit BLS field ops; run on n14"]
fn decode_bind_accepts_real_pubkeys() {
    let Some(v) = load_update() else {
        eprintln!("SKIP: no update fixture");
        return;
    };
    let pks = pubkeys(&v);
    let sample: Vec<[u8; 48]> = pks.into_iter().take(4).collect();

    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        for pk in &sample {
            let pt = load_point(ctx, range, decode_point(pk));
            let bs = load_bytes(ctx, pk);
            assert_pubkey_bytes_bind_point(range, ctx, &bs, &pt);
        }
    });
}

#[test]
#[ignore = "in-circuit; expected to reject (run on n14)"]
#[should_panic]
fn decode_bind_rejects_tampered_x() {
    let v = load_update().expect("fixture required for negative test");
    let pk = pubkeys(&v)[0];
    let pt_val = decode_point(&pk);
    let mut tampered = pk;
    tampered[20] ^= 0x01; // flip a byte inside the x-coordinate

    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let pt = load_point(ctx, range, pt_val);
        let bs = load_bytes(ctx, &tampered);
        assert_pubkey_bytes_bind_point(range, ctx, &bs, &pt);
    });
}

#[test]
#[ignore = "in-circuit; expected to reject (run on n14)"]
#[should_panic]
fn decode_bind_rejects_negated_y() {
    let v = load_update().expect("fixture required for negative test");
    let pk = pubkeys(&v)[0];
    let negated = -decode_point(&pk); // same x, y -> p-y; original bytes' sign bit no longer matches

    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let pt = load_point(ctx, range, negated);
        let bs = load_bytes(ctx, &pk);
        assert_pubkey_bytes_bind_point(range, ctx, &bs, &pt);
    });
}
