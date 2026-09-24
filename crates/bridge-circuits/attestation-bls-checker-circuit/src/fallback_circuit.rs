use std::cell::RefCell;
use std::collections::HashMap;

use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1Affine, G2Affine};
use halo2_base::utils::BigPrimeField;
use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
        GateInstructions, RangeInstructions,
    },
    halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        plonk::{Circuit, ConstraintSystem, Error},
    },
    AssignedValue, QuantumCell,
};
use halo2_ecc::bls12_381::{Fp2Chip, FpChip};
use halo2_ecc::ecc::hash_to_curve::{ExpandMsgXmd, HashToCurveChip};
use gosh_sha256_chip::Sha256Chip;
use gosh_bls_verification::{
    compute_all_pub_sum, load_bk_set_pubkeys,
    verify_bls_attestation_with_assigned_msghash, ThresholdMode,
};
use gosh_bls_verification::helpers::{deserialize_g1_pubkey, deserialize_g2_signature, DST};

use crate::attestation_data_parser::{
    parse_attestation_data_bytes, parse_signature_bytes, parse_signer_entries,
};
use bridge_poseidon::{compute_bk_set_commitment_padded, PADDING_SIGNER_INDEX};
use crate::{
    constraint_block_seqno_gt_last_seen,
    AttestationBlsCheckerCircuitParams, AttestationBlsCheckerConfig,
    ATTESTATION_DATA_LEN, BLOCK_SEQ_NO_REL_OFFSET, BLOCK_ID_REL_OFFSET,
    TARGET_TYPE_REL_OFFSET,
};

// ---------------------------------------------------------------------------
// Fallback constraint builder
// ---------------------------------------------------------------------------

/// Build all circuit constraints for Fallback finalization:
/// - Two attestations (primary-typed + fallback-typed) with matching block_id
/// - target_type checks: att1 == Primary (0x00000000), att2 == Fallback (0x01000000)
/// - Hash-to-curve + BLS verification for each attestation with > 50% threshold
/// - Poseidon(bk_set) → public instance [1]
/// - block_seq_no > last_seen_block_seqno → public instances [2], [3]
///
/// Returns (block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_seqno).
fn build_fallback_constraints<F: BigPrimeField>(
    builder: &mut BaseCircuitBuilder<F>,
    attestation_primary_bytes: &[u8],
    signature_primary: G2Affine,
    signer_entries_primary: &[(u16, u16)],
    attestation_fallback_bytes: &[u8],
    signature_fallback: G2Affine,
    signer_entries_fallback: &[(u16, u16)],
    bk_set_pubkeys: &[G1Affine],
    sorted_bk_set_indices: &[u16],
    max_signers: usize,
    limb_bits: usize,
    num_limbs: usize,
    last_seen_block_seqno: u32,
    actual_bk_set_size: usize,
) -> (AssignedValue<F>, AssignedValue<F>, AssignedValue<F>, AssignedValue<F>) {
    let range = builder.range_chip();

    // Load attestation 1 (Primary) bytes as witnesses (exactly ATTESTATION_DATA_LEN bytes).
    let attestation_data_1 =
        &parse_attestation_data_bytes(attestation_primary_bytes)[..ATTESTATION_DATA_LEN];
    let assigned_msg_1: Vec<AssignedValue<F>> = {
        let ctx = builder.main(0);
        attestation_data_1
            .iter()
            .map(|&b| ctx.load_witness(F::from(b as u64)))
            .collect()
    };

    // Load attestation 2 (Fallback) bytes as witnesses (exactly ATTESTATION_DATA_LEN bytes).
    let attestation_data_2 =
        &parse_attestation_data_bytes(attestation_fallback_bytes)[..ATTESTATION_DATA_LEN];
    let assigned_msg_2: Vec<AssignedValue<F>> = {
        let ctx = builder.main(0);
        attestation_data_2
            .iter()
            .map(|&b| ctx.load_witness(F::from(b as u64)))
            .collect()
    };

    // A. Extract block_id from attestation 1 and convert to Fr.
    //
    //    Reverse the 32 payload bytes before the base-256 inner product so
    //    Circuit 1B's `block_id_fr` = `uint256(bytes32(sha256_root))`. The
    //    payload holds the digest in its natural BE order (see the AN-node
    //    citations in `primary_circuit.rs::build_primary_constraints §A`),
    //    and this is the single `blockId` value `AckiNackiBridge.verifyBlock`
    //    feeds to *both* SNARK verifiers, so Circuit 1B must fold to the
    //    same Fr as Circuit 2. Halo2's `inner_product([256^i])` is LE, so
    //    reversing first yields the BE-integer reading.
    let block_id_fr = {
        let ctx = builder.main(0);
        let gate = range.gate();
        let block_id_cells_1 =
            &assigned_msg_1[BLOCK_ID_REL_OFFSET..BLOCK_ID_REL_OFFSET + 32];
        gate.inner_product(
            ctx,
            block_id_cells_1.iter().rev().map(|&b| QuantumCell::Existing(b)),
            (0..32).map(|i| QuantumCell::Constant(F::from(256u64).pow([i as u64]))),
        )
    };

    // A'. Constrain block_id bytes equal between both attestations.
    {
        let ctx = builder.main(0);
        for i in 0..32 {
            ctx.constrain_equal(
                &assigned_msg_1[BLOCK_ID_REL_OFFSET + i],
                &assigned_msg_2[BLOCK_ID_REL_OFFSET + i],
            );
        }
    }

    // A''. Constrain block_seq_no bytes equal between both attestations (defense-in-depth).
    {
        let ctx = builder.main(0);
        for i in 0..4 {
            ctx.constrain_equal(
                &assigned_msg_1[BLOCK_SEQ_NO_REL_OFFSET + i],
                &assigned_msg_2[BLOCK_SEQ_NO_REL_OFFSET + i],
            );
        }
    }

    // B. Constrain target_types.
    // Attestation 1: target_type == Primary (0x00000000).
    {
        let ctx = builder.main(0);
        let zero = ctx.load_constant(F::from(0u64));

        let tt1_byte0 = assigned_msg_1[TARGET_TYPE_REL_OFFSET];
        let tt1_byte1 = assigned_msg_1[TARGET_TYPE_REL_OFFSET + 1];
        let tt1_byte2 = assigned_msg_1[TARGET_TYPE_REL_OFFSET + 2];
        let tt1_byte3 = assigned_msg_1[TARGET_TYPE_REL_OFFSET + 3];

        ctx.constrain_equal(&tt1_byte0, &zero);
        ctx.constrain_equal(&tt1_byte1, &zero);
        ctx.constrain_equal(&tt1_byte2, &zero);
        ctx.constrain_equal(&tt1_byte3, &zero);
    }
    // Attestation 2: target_type == Fallback (0x01000000).
    {
        let ctx = builder.main(0);
        let zero = ctx.load_constant(F::from(0u64));
        let one = ctx.load_constant(F::from(1u64));

        let tt2_byte0 = assigned_msg_2[TARGET_TYPE_REL_OFFSET];
        let tt2_byte1 = assigned_msg_2[TARGET_TYPE_REL_OFFSET + 1];
        let tt2_byte2 = assigned_msg_2[TARGET_TYPE_REL_OFFSET + 2];
        let tt2_byte3 = assigned_msg_2[TARGET_TYPE_REL_OFFSET + 3];

        ctx.constrain_equal(&tt2_byte0, &one);
        ctx.constrain_equal(&tt2_byte1, &zero);
        ctx.constrain_equal(&tt2_byte2, &zero);
        ctx.constrain_equal(&tt2_byte3, &zero);
    }

    // B'. Constrain block_seq_no > last_seen_block_seqno (from primary attestation).
    let (block_seq_no_fr, last_seen_seqno) = constraint_block_seqno_gt_last_seen(
        builder, &range, &assigned_msg_1, last_seen_block_seqno,
    );

    // C. Load BK set pubkeys as assigned cells once, shared by both verifications.
    let assigned_pks = {
        let ctx = builder.main(0);
        load_bk_set_pubkeys(ctx, &range, bk_set_pubkeys, limb_bits, num_limbs)
    };

    // Compute all_pub_sum once, shared by both BLS verification calls.
    let all_pub_sum = {
        let ctx = builder.main(0);
        compute_all_pub_sum(ctx, &range, &assigned_pks, limb_bits, num_limbs)
    };

    // C'. Poseidon commitment to old BK set (yields n_real_pubkeys for threshold).
    //     Must happen before BLS verification so n_real_pubkeys is available.
    let (bk_set_commitment, n_real_pubkeys) = compute_bk_set_commitment_padded(
        builder,
        &range,
        &assigned_pks,
        sorted_bk_set_indices,
        num_limbs,
        actual_bk_set_size,
    );

    // D1. Hash-to-curve for attestation 1.
    let msghash_1_assigned;
    {
        let fp_chip = FpChip::new(&range, limb_bits, num_limbs);
        let fp2_chip = Fp2Chip::new(&fp_chip);
        let sha256_chip = Sha256Chip::new(&range);
        let h2c_chip = HashToCurveChip::new(&sha256_chip, &fp2_chip);

        let pool = builder.pool(0);
        msghash_1_assigned = h2c_chip
            .hash_to_curve::<ExpandMsgXmd>(
                pool,
                assigned_msg_1.into_iter().map(QuantumCell::Existing),
                DST,
            )
            .unwrap();
    }

    // E1. BLS verification for attestation 1 with Fallback threshold (> 50%).
    {
        let pool = builder.pool(0);
        verify_bls_attestation_with_assigned_msghash(
            pool,
            &range,
            signature_primary,
            msghash_1_assigned,
            &assigned_pks,
            signer_entries_primary,
            max_signers,
            limb_bits,
            num_limbs,
            ThresholdMode::Fallback,
            all_pub_sum.clone(),
            n_real_pubkeys,
        );
    }

    // D2. Hash-to-curve for attestation 2.
    let msghash_2_assigned;
    {
        let fp_chip = FpChip::new(&range, limb_bits, num_limbs);
        let fp2_chip = Fp2Chip::new(&fp_chip);
        let sha256_chip = Sha256Chip::new(&range);
        let h2c_chip = HashToCurveChip::new(&sha256_chip, &fp2_chip);

        let pool = builder.pool(0);
        msghash_2_assigned = h2c_chip
            .hash_to_curve::<ExpandMsgXmd>(
                pool,
                assigned_msg_2.into_iter().map(QuantumCell::Existing),
                DST,
            )
            .unwrap();
    }

    // E2. BLS verification for attestation 2 with Fallback threshold (> 50%).
    {
        let pool = builder.pool(0);
        verify_bls_attestation_with_assigned_msghash(
            pool,
            &range,
            signature_fallback,
            msghash_2_assigned,
            &assigned_pks,
            signer_entries_fallback,
            max_signers,
            limb_bits,
            num_limbs,
            ThresholdMode::Fallback,
            all_pub_sum,
            n_real_pubkeys,
        );
    }

    (block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_seqno)
}

// ---------------------------------------------------------------------------
// FallbackAttestationBlsCheckerCircuit
// ---------------------------------------------------------------------------

pub struct FallbackAttestationBlsCheckerCircuit<F: BigPrimeField> {
    pub attestation_primary_bytes: Vec<u8>,
    pub attestation_fallback_bytes: Vec<u8>,
    pub bk_set: HashMap<u16, Vec<u8>>,
    pub last_seen_block_seqno: u32,
    pub params: AttestationBlsCheckerCircuitParams,
    signature_primary: G2Affine,
    signature_fallback: G2Affine,
    bk_set_pubkeys: Vec<G1Affine>,
    sorted_bk_set_indices: Vec<u16>,
    actual_bk_set_size: usize,
    signer_entries_primary: Vec<(u16, u16)>,
    signer_entries_fallback: Vec<(u16, u16)>,
    base_circuit_builder: RefCell<BaseCircuitBuilder<F>>,
}

impl<F: BigPrimeField> FallbackAttestationBlsCheckerCircuit<F> {
    pub fn new(
        attestation_primary_bytes: Vec<u8>,
        attestation_fallback_bytes: Vec<u8>,
        bk_set: HashMap<u16, Vec<u8>>,
        last_seen_block_seqno: u32,
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
        limb_bits: usize,
        num_limbs: usize,
        max_signers: usize,
    ) -> Self {
        let sig_bytes_primary = parse_signature_bytes(&attestation_primary_bytes);
        let signature_primary = deserialize_g2_signature(sig_bytes_primary);
        let sig_bytes_fallback = parse_signature_bytes(&attestation_fallback_bytes);
        let signature_fallback = deserialize_g2_signature(sig_bytes_fallback);

        let mut sorted_keys: Vec<u16> = bk_set.keys().cloned().collect();
        sorted_keys.sort();
        let actual_bk_set_size = sorted_keys.len();
        let mut bk_set_pubkeys: Vec<G1Affine> = sorted_keys
            .iter()
            .map(|k| deserialize_g1_pubkey(&bk_set[k]))
            .collect();

        // Pad BK set to max_signers for fixed circuit structure (single VK).
        bk_set_pubkeys.resize(max_signers, G1Affine::generator());
        sorted_keys.resize(max_signers, PADDING_SIGNER_INDEX);

        // Remap original protocol-level signer indices to sorted array positions.
        // idx_to_indicator in the BLS verification selects pubkeys by array position,
        // not by original u16 key. Without remapping, non-contiguous BK set indices
        // (e.g. {10, 20, 30}) would select wrong (padding) pubkeys.
        let remap = |entries: Vec<(u16, u16)>| -> Vec<(u16, u16)> {
            let mut remapped: Vec<(u16, u16)> = entries
                .into_iter()
                .map(|(orig_idx, count)| {
                    let pos = sorted_keys[..actual_bk_set_size]
                        .iter()
                        .position(|&k| k == orig_idx)
                        .unwrap_or_else(|| panic!("signer index {} not in bk_set", orig_idx));
                    (pos as u16, count)
                })
                .collect();
            remapped.sort_by_key(|&(idx, _)| idx);
            remapped
        };
        let signer_entries_primary = remap(parse_signer_entries(&attestation_primary_bytes));
        let signer_entries_fallback = remap(parse_signer_entries(&attestation_fallback_bytes));

        // Simulate witness generation to determine BaseCircuitParams.
        let base_circuit_params = Self::calculate_base_circuit_params(
            k,
            num_unusable_rows,
            lookup_bits,
            limb_bits,
            num_limbs,
            &attestation_primary_bytes,
            signature_primary,
            &signer_entries_primary,
            &attestation_fallback_bytes,
            signature_fallback,
            &signer_entries_fallback,
            &bk_set_pubkeys,
            &sorted_keys,
            max_signers,
            last_seen_block_seqno,
            actual_bk_set_size,
        );

        let params = AttestationBlsCheckerCircuitParams {
            k,
            num_unusable_rows,
            base_circuit_params: base_circuit_params.clone(),
            limb_bits,
            num_limbs,
            max_signers,
        };

        let mut base_circuit_builder = BaseCircuitBuilder::new(false);
        base_circuit_builder.set_params(base_circuit_params);

        Self {
            attestation_primary_bytes,
            attestation_fallback_bytes,
            bk_set,
            last_seen_block_seqno,
            params,
            signature_primary,
            signature_fallback,
            bk_set_pubkeys,
            sorted_bk_set_indices: sorted_keys,
            actual_bk_set_size,
            signer_entries_primary,
            signer_entries_fallback,
            base_circuit_builder: RefCell::new(base_circuit_builder),
        }
    }

    /// Override the base circuit params (e.g., to use a shared vk/pk).
    pub fn override_base_circuit_params(&mut self, base_params: BaseCircuitParams) {
        self.params.base_circuit_params = base_params.clone();
        self.base_circuit_builder.borrow_mut().set_params(base_params);
    }

    #[allow(clippy::too_many_arguments)]
    fn calculate_base_circuit_params(
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
        limb_bits: usize,
        num_limbs: usize,
        attestation_primary_bytes: &[u8],
        signature_primary: G2Affine,
        signer_entries_primary: &[(u16, u16)],
        attestation_fallback_bytes: &[u8],
        signature_fallback: G2Affine,
        signer_entries_fallback: &[(u16, u16)],
        bk_set_pubkeys: &[G1Affine],
        sorted_bk_set_indices: &[u16],
        max_signers: usize,
        last_seen_block_seqno: u32,
        actual_bk_set_size: usize,
    ) -> BaseCircuitParams {
        let mut builder = BaseCircuitBuilder::<F>::new(false);
        builder.set_params(BaseCircuitParams {
            k,
            num_instance_columns: 1,
            lookup_bits: Some(lookup_bits),
            ..Default::default()
        });

        build_fallback_constraints(
            &mut builder,
            attestation_primary_bytes,
            signature_primary,
            signer_entries_primary,
            attestation_fallback_bytes,
            signature_fallback,
            signer_entries_fallback,
            bk_set_pubkeys,
            sorted_bk_set_indices,
            max_signers,
            limb_bits,
            num_limbs,
            last_seen_block_seqno,
            actual_bk_set_size,
        );

        let params = builder.calculate_params(Some(num_unusable_rows));
        builder.clear();
        params
    }

    fn generate_witnesses(&self) {
        let mut builder = self.base_circuit_builder.borrow_mut();

        while builder.assigned_instances.len()
            < self.params.base_circuit_params.num_instance_columns
        {
            builder.assigned_instances.push(vec![]);
        }

        let (block_id_fr, old_commitment, block_seq_no_fr, last_seen_seqno) =
            build_fallback_constraints(
                &mut builder,
                &self.attestation_primary_bytes,
                self.signature_primary,
                &self.signer_entries_primary,
                &self.attestation_fallback_bytes,
                self.signature_fallback,
                &self.signer_entries_fallback,
                &self.bk_set_pubkeys,
                &self.sorted_bk_set_indices,
                self.params.max_signers,
                self.params.limb_bits,
                self.params.num_limbs,
                self.last_seen_block_seqno,
                self.actual_bk_set_size,
            );

        // Public instances:
        // [0] = block_id, [1] = Poseidon(bk_set),
        // [2] = block_seq_no,  [3] = last_seen_block_seqno
        builder.assigned_instances[0].push(block_id_fr);
        builder.assigned_instances[0].push(old_commitment);
        builder.assigned_instances[0].push(block_seq_no_fr);
        builder.assigned_instances[0].push(last_seen_seqno);
    }
}

impl<F: BigPrimeField> Circuit<F> for FallbackAttestationBlsCheckerCircuit<F> {
    type Config = AttestationBlsCheckerConfig<F>;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = AttestationBlsCheckerCircuitParams;

    fn params(&self) -> Self::Params {
        self.params.clone()
    }

    fn without_witnesses(&self) -> Self {
        unimplemented!()
    }

    fn configure_with_params(meta: &mut ConstraintSystem<F>, params: Self::Params) -> Self::Config {
        AttestationBlsCheckerConfig::configure_with_params(meta, params.base_circuit_params)
    }

    fn configure(_: &mut ConstraintSystem<F>) -> Self::Config {
        unreachable!("Use configure_with_params")
    }

    fn synthesize(
        &self,
        config: Self::Config,
        layouter: impl Layouter<F>,
    ) -> Result<(), Error> {
        self.generate_witnesses();
        self.base_circuit_builder
            .borrow()
            .synthesize(config.base_config, layouter)?;
        self.base_circuit_builder.borrow_mut().clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{K, LOOKUP_BITS, NUM_UNUSABLE_ROWS};
    use bridge_poseidon::{LIMB_BITS, MAX_SIGNERS, NUM_LIMBS};
    use gosh_bls_verification::helpers::{
        compute_agg_pubkey, compute_msg_hash, resolve_pubkeys, verify_bls_native,
    };
    use halo2_base::halo2_proofs::{dev::MockProver, halo2curves::bn256::Fr};

    use crate::test_instances::expected_public_instances;

    use bridge_test_data_gen::generator::TestData;
    use std::time::Instant;

    /// One "should-pass" mock case for the fallback circuit: a label + a
    /// closure that materialises the (primary + fallback) test data
    /// (panicking on failure) + the `max_signers` padding and `k` to build
    /// the circuit with.
    struct FallbackMockCase {
        label: &'static str,
        make_test_data: Box<dyn Fn() -> TestData>,
        max_signers: usize,
        k: u32,
    }

    fn fallback_mock_cases() -> Vec<FallbackMockCase> {
        vec![
            FallbackMockCase {
                label: "10 signers, all sign both",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(10)
                        .expect("generate_test_data_fallback_all_sign failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
            FallbackMockCase {
                label: "fallback threshold 6/10 each",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_fallback_threshold(10)
                        .expect("generate_test_data_fallback_threshold failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
        ]
    }

    /// Drive a single fallback-circuit mock case end-to-end:
    /// off-circuit BLS check (both attestations) → build circuit →
    /// MockProver run + assert.
    fn run_fallback_mock_case(case: &FallbackMockCase) {
        let t_total = Instant::now();
        let test_data = (case.make_test_data)();
        let att_primary = test_data.attestation_bytes.clone();
        let att_fallback = test_data
            .attestation_2_bytes
            .clone()
            .expect("fallback test data must have attestation_2_bytes");
        let bk_set = test_data.bk_set.clone();

        println!(
            "Loaded test data: att_primary={} bytes, att_fallback={} bytes, bk_set={} keys",
            att_primary.len(),
            att_fallback.len(),
            bk_set.len()
        );

        // Off-circuit BLS verification (both attestations).
        let t = Instant::now();
        for (lbl, att) in [("primary", &att_primary), ("fallback", &att_fallback)] {
            let sig_bytes = parse_signature_bytes(att);
            let entries = parse_signer_entries(att);
            let msg_bytes = parse_attestation_data_bytes(att);
            let signature = deserialize_g2_signature(sig_bytes);
            let msg_hash = compute_msg_hash(msg_bytes);
            let pubkeys_with_counts = resolve_pubkeys(&entries, &bk_set);
            let agg_pk = compute_agg_pubkey(&pubkeys_with_counts);
            assert!(
                verify_bls_native(&signature, &agg_pk, &msg_hash),
                "Off-circuit BLS verification failed [{} / {}]",
                case.label,
                lbl
            );
        }
        println!("[timing] off-circuit BLS verification (both): {:?}", t.elapsed());

        let (last_seen_block_seqno, instances) =
            expected_public_instances(&att_primary, &bk_set, case.max_signers);
        println!(
            "block_seq_no = {}, last_seen = {}",
            last_seen_block_seqno + 1,
            last_seen_block_seqno
        );

        let lookup_bits = case.k as usize - 1;
        let t = Instant::now();
        let circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
            att_primary,
            att_fallback,
            bk_set,
            last_seen_block_seqno,
            case.k as usize,
            NUM_UNUSABLE_ROWS,
            lookup_bits,
            LIMB_BITS,
            NUM_LIMBS,
            case.max_signers,
        );
        println!("[timing] circuit construction: {:?}", t.elapsed());
        println!(
            "base_circuit_params: {:?}",
            circuit.params.base_circuit_params
        );

        println!("Running MockProver at K={}...", case.k);
        let t = Instant::now();
        let prover = MockProver::run(case.k, &circuit, vec![instances])
            .unwrap_or_else(|e| panic!("MockProver::run failed [{}]: {e:?}", case.label));
        println!("[timing] MockProver::run: {:?}", t.elapsed());

        let t = Instant::now();
        prover.assert_satisfied();
        println!("[timing] MockProver::assert_satisfied: {:?}", t.elapsed());
        println!("[timing] case TOTAL: {:?}", t_total.elapsed());
        println!("MockProver passed [{}]!", case.label);
    }

    /// Table-driven mock-prover sanity check across fallback-circuit test
    /// cases. Add new should-pass cases to `fallback_mock_cases()` rather
    /// than writing a new `#[test]` function.
    #[test]
    fn test_fallback_attestation_bls_checker_mock() {
        for case in fallback_mock_cases() {
            println!("\n========== {} ==========", case.label);
            run_fallback_mock_case(&case);
        }
    }

    /// Fallback with <=50% signers should FAIL (needs >50%). Stays separate
    /// from the table-driven suite because it asserts a panic.
    #[test]
    #[should_panic]
    fn test_fallback_insufficient_signers_fails() {
        let test_data =
            bridge_test_data_gen::generator::generate_test_data_fallback_below_threshold(10)
                .expect("generate_test_data_fallback_below_threshold failed");

        let (last_seen_block_seqno, instances) = expected_public_instances(
            &test_data.attestation_bytes,
            &test_data.bk_set,
            MAX_SIGNERS,
        );

        let circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            test_data.attestation_2_bytes.unwrap(),
            test_data.bk_set,
            last_seen_block_seqno,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );

        let prover = MockProver::run(K, &circuit, vec![instances]).unwrap();
        // This should panic: 5 signers doesn't meet >50% of 10.
        prover.assert_satisfied();
    }
}
