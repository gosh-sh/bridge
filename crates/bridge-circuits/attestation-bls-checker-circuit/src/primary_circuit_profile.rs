//! Cell-cost profiling for the Primary attestation BLS checker circuit.
//!
//! Kept in a dedicated module (compiled only under `#[cfg(test)]`) so the
//! large profiling test does not clutter `primary_circuit.rs`.
//!
//! Two passes:
//!   1. **Instrumented body** — replays each circuit section manually so we
//!      can call `builder.statistics()` between sections and report deltas.
//!      Intentionally mirrors `build_primary_constraints` step-for-step:
//!      keep section ordering in sync if either side changes.
//!   2. **K sweep** — for k ∈ [18, 19, 20, 21], builds the full circuit via
//!      `build_primary_constraints` (no duplication) and prints what
//!      `calculate_params` suggests.
//!
//! Run: `cargo test -p attestation-bls-checker-circuit test_profile_cell_costs -- --nocapture`

use std::time::Instant;

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::gates::{GateInstructions, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::Field;
use halo2_base::QuantumCell;
use halo2_ecc::bls12_381::{Fp2Chip, FpChip};
use halo2_ecc::ecc::hash_to_curve::{ExpandMsgXmd, HashToCurveChip};

use gosh_sha256_chip::Sha256Chip;
use gosh_bls_verification::{
    compute_all_pub_sum, load_bk_set_pubkeys,
    verify_bls_attestation_with_assigned_msghash, ThresholdMode,
};
use gosh_bls_verification::helpers::{deserialize_g1_pubkey, deserialize_g2_signature, DST};

use bridge_poseidon::{
    compute_bk_set_commitment_padded, LIMB_BITS, MAX_SIGNERS, NUM_LIMBS, PADDING_SIGNER_INDEX,
};

use crate::attestation_data_parser::{parse_attestation_data_bytes, parse_signature_bytes, parse_signer_entries};
use crate::primary_circuit::build_primary_constraints;
use crate::{
    constraint_block_seqno_gt_last_seen, ATTESTATION_DATA_LEN, BLOCK_ID_REL_OFFSET,
    NUM_UNUSABLE_ROWS, TARGET_TYPE_REL_OFFSET,
};

/// Profile cell costs per section and report `calculate_params` for K ∈ [18..21].
#[test]
fn test_profile_cell_costs() {
    let test_data = bridge_test_data_gen::generator::generate_test_data_all_sign(10)
        .expect("generate_test_data_all_sign failed");

    // -- Prepare inputs (same shape as PrimaryAttestationBlsCheckerCircuit::new) --
    let mut sorted_keys: Vec<u16> = test_data.bk_set.keys().cloned().collect();
    sorted_keys.sort();
    let actual_bk_set_size = sorted_keys.len();
    let mut bk_set_pubkeys: Vec<G1Affine> = sorted_keys
        .iter()
        .map(|k| deserialize_g1_pubkey(&test_data.bk_set[k]))
        .collect();
    bk_set_pubkeys.resize(MAX_SIGNERS, G1Affine::generator());
    sorted_keys.resize(MAX_SIGNERS, PADDING_SIGNER_INDEX);

    let sig_bytes = parse_signature_bytes(&test_data.attestation_bytes);
    let signature = deserialize_g2_signature(sig_bytes);

    let signer_entries_raw = parse_signer_entries(&test_data.attestation_bytes);
    let mut signer_entries: Vec<(u16, u16)> = signer_entries_raw
        .into_iter()
        .map(|(orig_idx, count)| {
            let pos = sorted_keys[..actual_bk_set_size]
                .iter()
                .position(|&k| k == orig_idx)
                .unwrap();
            (pos as u16, count)
        })
        .collect();
    signer_entries.sort_by_key(|&(idx, _)| idx);

    let attestation_data =
        &parse_attestation_data_bytes(&test_data.attestation_bytes)[..ATTESTATION_DATA_LEN];

    // ────────────────────────────────────────────────────────────────────────
    // Pass 1: Instrumented body — section-by-section advice-cell snapshots.
    //
    // This deliberately open-codes the same flow as `build_primary_constraints`
    // because we need `builder.statistics()` *between* sections. Keep ordering
    // in sync with `build_primary_constraints` (load → block_id + target_type
    // + seqno → load_pks → all_pub_sum → poseidon → hash_to_curve → BLS).
    // ────────────────────────────────────────────────────────────────────────
    let k_test = 20usize;
    let lookup_bits = k_test - 1;
    let mut builder = BaseCircuitBuilder::<Fr>::new(false);
    builder.set_params(BaseCircuitParams {
        k: k_test,
        num_instance_columns: 1,
        lookup_bits: Some(lookup_bits),
        ..Default::default()
    });

    let range = builder.range_chip();

    // -- Section: load attestation bytes --
    let assigned_msg: Vec<_> = {
        let ctx = builder.main(0);
        attestation_data
            .iter()
            .map(|&b| ctx.load_witness(Fr::from(b as u64)))
            .collect()
    };
    let s0 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] load attestation bytes: {}", s0);

    // -- Section: block_id + target_type + block_seq_no --
    let _block_id_fr = {
        let ctx = builder.main(0);
        let gate = range.gate();
        let cells = &assigned_msg[BLOCK_ID_REL_OFFSET..BLOCK_ID_REL_OFFSET + 32];
        gate.inner_product(
            ctx,
            cells.iter().map(|&b| QuantumCell::Existing(b)),
            (0..32).map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64]))),
        )
    };
    {
        let ctx = builder.main(0);
        let zero = ctx.load_constant(Fr::from(0u64));
        for i in 0..4 {
            ctx.constrain_equal(&assigned_msg[TARGET_TYPE_REL_OFFSET + i], &zero);
        }
    }
    let _seqno = constraint_block_seqno_gt_last_seen(&mut builder, &range, &assigned_msg, 0);
    let s1 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] field extraction + constraints: {} (delta: {})", s1, s1 - s0);

    // -- Section: load BK set pubkeys --
    let assigned_pks = {
        let ctx = builder.main(0);
        load_bk_set_pubkeys(ctx, &range, &bk_set_pubkeys, LIMB_BITS, NUM_LIMBS)
    };
    let s2 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] load_bk_set_pubkeys ({}): {} (delta: {})", MAX_SIGNERS, s2, s2 - s1);

    // -- Section: compute_all_pub_sum --
    let t = Instant::now();
    let all_pub_sum = {
        let ctx = builder.main(0);
        compute_all_pub_sum(ctx, &range, &assigned_pks, LIMB_BITS, NUM_LIMBS)
    };
    let s3 = builder.statistics().gate.total_advice_per_phase[0];
    println!(
        "[cells] compute_all_pub_sum ({}): {} (delta: {}) [{:?}]",
        MAX_SIGNERS, s3, s3 - s2, t.elapsed()
    );

    // -- Section: Poseidon commitment --
    let (_commitment, n_real) = compute_bk_set_commitment_padded(
        &mut builder,
        &range,
        &assigned_pks,
        &sorted_keys,
        NUM_LIMBS,
        actual_bk_set_size,
    );
    let s4 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] poseidon commitment ({}): {} (delta: {})", MAX_SIGNERS, s4, s4 - s3);

    // -- Section: hash_to_curve --
    let t = Instant::now();
    let msghash_assigned;
    {
        let fp_chip = FpChip::new(&range, LIMB_BITS, NUM_LIMBS);
        let fp2_chip = Fp2Chip::new(&fp_chip);
        let sha256_chip = Sha256Chip::new(&range);
        let h2c_chip = HashToCurveChip::new(&sha256_chip, &fp2_chip);
        let pool = builder.pool(0);
        msghash_assigned = h2c_chip
            .hash_to_curve::<ExpandMsgXmd>(
                pool,
                assigned_msg.into_iter().map(QuantumCell::Existing),
                DST,
            )
            .unwrap();
    }
    let s5 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] hash_to_curve: {} (delta: {}) [{:?}]", s5, s5 - s4, t.elapsed());

    // -- Section: BLS verification --
    let t = Instant::now();
    {
        let pool = builder.pool(0);
        verify_bls_attestation_with_assigned_msghash(
            pool,
            &range,
            signature,
            msghash_assigned,
            &assigned_pks,
            &signer_entries,
            MAX_SIGNERS,
            LIMB_BITS,
            NUM_LIMBS,
            ThresholdMode::Primary,
            all_pub_sum,
            n_real,
        );
    }
    let s6 = builder.statistics().gate.total_advice_per_phase[0];
    println!("[cells] BLS verification: {} (delta: {}) [{:?}]", s6, s6 - s5, t.elapsed());

    let lookup_cells = builder.statistics().total_lookup_advice_per_phase[0];
    println!("\n=== TOTAL ===");
    println!("  advice cells:  {}", s6);
    println!("  lookup cells:  {}", lookup_cells);
    println!("  total cells:   {}", s6 + lookup_cells);

    // ────────────────────────────────────────────────────────────────────────
    // Pass 2: K sweep — full circuit via build_primary_constraints, then
    // print calculate_params output.
    // ────────────────────────────────────────────────────────────────────────
    for k in [18, 19, 20, 21] {
        let mut b2 = BaseCircuitBuilder::<Fr>::new(false);
        b2.set_params(BaseCircuitParams {
            k,
            num_instance_columns: 1,
            lookup_bits: Some(k - 1),
            ..Default::default()
        });

        build_primary_constraints(
            &mut b2,
            &test_data.attestation_bytes,
            signature,
            &signer_entries,
            &bk_set_pubkeys,
            &sorted_keys,
            MAX_SIGNERS,
            LIMB_BITS,
            NUM_LIMBS,
            0,
            actual_bk_set_size,
        );

        let params = b2.calculate_params(Some(NUM_UNUSABLE_ROWS));
        let adv: usize = params.num_advice_per_phase.iter().sum();
        let lkp: usize = params.num_lookup_advice_per_phase.iter().sum();
        let rows = (1usize << k) - NUM_UNUSABLE_ROWS;
        println!(
            "K={}: {} advice cols + {} lookup cols = {} total cols, {} usable rows, capacity={}",
            k, adv, lkp, adv + lkp, rows, (adv + lkp) * rows
        );
    }
}
