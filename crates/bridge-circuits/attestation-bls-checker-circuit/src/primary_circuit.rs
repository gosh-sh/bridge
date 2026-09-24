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
    ATTESTATION_DATA_LEN, BLOCK_ID_REL_OFFSET,
    TARGET_TYPE_REL_OFFSET,
};

// ---------------------------------------------------------------------------
// Core constraint builder
// ---------------------------------------------------------------------------

/// Build all circuit constraints for Primary finalization:
/// - Extract block_id from attestation → public instance [0]
/// - target_type == Primary (0x00000000)
/// - Hash-to-curve + BLS verification with >= ceil(2n/3) threshold
/// - Poseidon(bk_set) → public instance [1]
/// - block_seq_no > last_seen_block_seqno → public instances [2], [3]
///
/// Returns (block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_seqno).
pub(crate) fn build_primary_constraints<F: BigPrimeField>(
    builder: &mut BaseCircuitBuilder<F>,
    attestation_bytes: &[u8],
    signature: G2Affine,
    signer_entries: &[(u16, u16)],
    bk_set_pubkeys: &[G1Affine],
    sorted_bk_set_indices: &[u16],
    max_signers: usize,
    limb_bits: usize,
    num_limbs: usize,
    last_seen_block_seqno: u32,
    actual_bk_set_size: usize,
) -> (AssignedValue<F>, AssignedValue<F>, AssignedValue<F>, AssignedValue<F>) {
    let range = builder.range_chip();

    // Load attestation_data bytes as witnesses (exactly ATTESTATION_DATA_LEN bytes).
    let attestation_data =
        &parse_attestation_data_bytes(attestation_bytes)[..ATTESTATION_DATA_LEN];
    let assigned_msg: Vec<AssignedValue<F>> = {
        let ctx = builder.main(0);
        attestation_data
            .iter()
            .map(|&b| ctx.load_witness(F::from(b as u64)))
            .collect()
    };

    // A. Extract block_id from attestation and convert to Fr.
    //
    //    Canonical `block_id` interpretation across the whole system is
    //    `uint256(bytes32(sha256_root))` — the natural integer value of the
    //    big-endian SHA-256 digest. Evidence:
    //
    //    * AN node computation
    //      (`acki-nacki/node/src/types/ackinacki_block/merkle.rs`):
    //      `hasher.finalize().into()` — raw BE bytes, no reversal.
    //    * AN external emission (`node/libs/node-types/src/u256.rs`):
    //      `hex::encode(self.0)` on those BE bytes → the hex string the
    //      relayer sees is `hex(sha256_output)`.
    //    * AN bincode of `BlockIdentifier` inside `AttestationData`
    //      (`node/src/node/associated_types.rs` + `u256.rs` `ser = bytes`):
    //      the same BE bytes are written verbatim into the attestation
    //      payload we read here — so `assigned_msg[BLOCK_ID_REL_OFFSET..]`
    //      contains the SHA-256 digest in its natural BE byte order.
    //    * On-chain (`AckiNackiBridge.verifyBlock(bytes32 blockId, ...)`):
    //      the single `blockId` argument is fed to *both*
    //      `primaryVerifier.verifyPrimaryAttestation` and
    //      `layerHashesVerifier.verifyLayerHashesMovement`. Solidity's
    //      `uint256(bytes32)` cast is the natural BE-integer reading — the
    //      same as `Σ byte[i] · 256^(31 - i)`.
    //    * Circuit 2 (`historical-layer-hashes-movement-checker`) folds
    //      `reverse(sha256_root)` with `Σ byte[i] · 256^i`, producing
    //      exactly that value.
    //
    //    Halo2's `inner_product(bytes, [256^i])` is a *little-endian* fold;
    //    to obtain the BE-integer reading of natural-BE payload bytes we
    //    reverse them first. This makes Circuit 1's `block_id_fr` equal to
    //    Circuit 2's and to `uint256(bytes32(blockId))` as fed on-chain.
    let block_id_fr = {
        let ctx = builder.main(0);
        let gate = range.gate();
        let block_id_cells =
            &assigned_msg[BLOCK_ID_REL_OFFSET..BLOCK_ID_REL_OFFSET + 32];
        gate.inner_product(
            ctx,
            block_id_cells.iter().rev().map(|&b| QuantumCell::Existing(b)),
            (0..32).map(|i| QuantumCell::Constant(F::from(256u64).pow([i as u64]))),
        )
    };

    // A'. Constrain target_type == Primary (0x00000000).
    {
        let ctx = builder.main(0);
        let zero = ctx.load_constant(F::from(0u64));

        let tt_byte0 = assigned_msg[TARGET_TYPE_REL_OFFSET];
        let tt_byte1 = assigned_msg[TARGET_TYPE_REL_OFFSET + 1];
        let tt_byte2 = assigned_msg[TARGET_TYPE_REL_OFFSET + 2];
        let tt_byte3 = assigned_msg[TARGET_TYPE_REL_OFFSET + 3];

        ctx.constrain_equal(&tt_byte0, &zero);
        ctx.constrain_equal(&tt_byte1, &zero);
        ctx.constrain_equal(&tt_byte2, &zero);
        ctx.constrain_equal(&tt_byte3, &zero);
    }

    // A''. Constrain block_seq_no > last_seen_block_seqno.
    let (block_seq_no_fr, last_seen_seqno) = constraint_block_seqno_gt_last_seen(
        builder, &range, &assigned_msg, last_seen_block_seqno,
    );

    // B. Load BK set pubkeys as assigned cells.
    let assigned_pks = {
        let ctx = builder.main(0);
        load_bk_set_pubkeys(ctx, &range, bk_set_pubkeys, limb_bits, num_limbs)
    };

    // Compute all_pub_sum for post-MSM correction in BLS verification.
    let all_pub_sum = {
        let ctx = builder.main(0);
        compute_all_pub_sum(ctx, &range, &assigned_pks, limb_bits, num_limbs)
    };

    // C. Poseidon commitment to old BK set (also yields n_real_pubkeys for threshold).
    //    Must happen before BLS verification so n_real_pubkeys is available.
    let (bk_set_commitment, n_real_pubkeys) = compute_bk_set_commitment_padded(
        builder,
        &range,
        &assigned_pks,
        sorted_bk_set_indices,
        num_limbs,
        actual_bk_set_size,
    );

    // D. Hash-to-curve for the attestation.
    let msghash_assigned;
    {
        let fp_chip = FpChip::new(&range, limb_bits, num_limbs);
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

    // E. BLS verification with Primary threshold (>= ceil(2n/3)).
    {
        let pool = builder.pool(0);
        verify_bls_attestation_with_assigned_msghash(
            pool,
            &range,
            signature,
            msghash_assigned,
            &assigned_pks,
            signer_entries,
            max_signers,
            limb_bits,
            num_limbs,
            ThresholdMode::Primary,
            all_pub_sum,
            n_real_pubkeys,
        );
    }

    (block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_seqno)
}

// ---------------------------------------------------------------------------
// PrimaryAttestationBlsCheckerCircuit
// ---------------------------------------------------------------------------

pub struct PrimaryAttestationBlsCheckerCircuit<F: BigPrimeField> {
    pub attestation_bytes: Vec<u8>,
    pub bk_set: HashMap<u16, Vec<u8>>,
    pub last_seen_block_seqno: u32,
    pub params: AttestationBlsCheckerCircuitParams,
    signature: G2Affine,
    bk_set_pubkeys: Vec<G1Affine>,
    sorted_bk_set_indices: Vec<u16>,
    actual_bk_set_size: usize,
    signer_entries: Vec<(u16, u16)>,
    base_circuit_builder: RefCell<BaseCircuitBuilder<F>>,
}

impl<F: BigPrimeField> PrimaryAttestationBlsCheckerCircuit<F> {
    pub fn new(
        attestation_bytes: Vec<u8>,
        bk_set: HashMap<u16, Vec<u8>>,
        last_seen_block_seqno: u32,
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
        limb_bits: usize,
        num_limbs: usize,
        max_signers: usize,
    ) -> Self {
        let sig_bytes = parse_signature_bytes(&attestation_bytes);
        let signature = deserialize_g2_signature(sig_bytes);

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

        let signer_entries = parse_signer_entries(&attestation_bytes);
        // Remap original protocol-level signer indices to sorted array positions.
        // idx_to_indicator in the BLS verification selects pubkeys by array position,
        // not by original u16 key. Without remapping, non-contiguous BK set indices
        // (e.g. {10, 20, 30}) would select wrong (padding) pubkeys.
        let mut signer_entries: Vec<(u16, u16)> = signer_entries
            .into_iter()
            .map(|(orig_idx, count)| {
                let pos = sorted_keys[..actual_bk_set_size]
                    .iter()
                    .position(|&k| k == orig_idx)
                    .unwrap_or_else(|| panic!("signer index {} not in bk_set", orig_idx));
                (pos as u16, count)
            })
            .collect();
        signer_entries.sort_by_key(|&(idx, _)| idx);

        // Simulate witness generation to determine BaseCircuitParams.
        let base_circuit_params = Self::calculate_base_circuit_params(
            k,
            num_unusable_rows,
            lookup_bits,
            limb_bits,
            num_limbs,
            &attestation_bytes,
            signature,
            &signer_entries,
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
            attestation_bytes,
            bk_set,
            last_seen_block_seqno,
            params,
            signature,
            bk_set_pubkeys,
            sorted_bk_set_indices: sorted_keys,
            actual_bk_set_size,
            signer_entries,
            base_circuit_builder: RefCell::new(base_circuit_builder),
        }
    }

    /// Override the base circuit params (e.g., to use a shared vk/pk).
    pub fn override_base_circuit_params(&mut self, base_params: BaseCircuitParams) {
        self.params.base_circuit_params = base_params.clone();
        self.base_circuit_builder.borrow_mut().set_params(base_params);
    }

    fn calculate_base_circuit_params(
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
        limb_bits: usize,
        num_limbs: usize,
        attestation_bytes: &[u8],
        signature: G2Affine,
        signer_entries: &[(u16, u16)],
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

        build_primary_constraints(
            &mut builder,
            attestation_bytes,
            signature,
            signer_entries,
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
            build_primary_constraints(
                &mut builder,
                &self.attestation_bytes,
                self.signature,
                &self.signer_entries,
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

impl<F: BigPrimeField> Circuit<F> for PrimaryAttestationBlsCheckerCircuit<F> {
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

    /// One "should-pass" mock case for the primary circuit: a label + a
    /// closure that materialises the test data (panicking on failure) +
    /// the `max_signers` padding and `k` to build the circuit with.
    struct PrimaryMockCase {
        label: &'static str,
        make_test_data: Box<dyn Fn() -> TestData>,
        max_signers: usize,
        k: u32,
    }

    fn primary_mock_cases() -> Vec<PrimaryMockCase> {
        vec![
            PrimaryMockCase {
                label: "10 signers, all sign",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(10)
                        .expect("generate_test_data_all_sign failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
            PrimaryMockCase {
                label: "primary threshold 7/10",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_primary_threshold(10)
                        .expect("generate_test_data_primary_threshold failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
            PrimaryMockCase {
                label: "10 signers, non-contiguous indices {5,10,...,50}",
                make_test_data: Box::new(|| {
                    let indices: Vec<u16> = (0..10).map(|i| 5 + i * 5).collect();
                    bridge_test_data_gen::generator::generate_test_data_all_sign_custom_indices(
                        &indices,
                    )
                    .expect("generate_test_data_all_sign_custom_indices failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
            PrimaryMockCase {
                label: "300 signers, all sign (full MAX_SIGNERS capacity)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(MAX_SIGNERS)
                        .expect("generate_test_data_all_sign(MAX_SIGNERS) failed")
                }),
                max_signers: MAX_SIGNERS,
                k: K,
            },
            PrimaryMockCase {
                label: "400 signers, all sign (scaling)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(400)
                        .expect("generate_test_data_all_sign(400) failed")
                }),
                max_signers: 400,
                k: K,
            },
        ]
    }

    /// Heavy scaling cases — gated behind `#[ignore]` so the default suite
    /// stays fast. Run with `-- --ignored --nocapture`. Each case sets its own
    /// `k`; bump to 21 if a case hits the K=20 column ceiling.
    fn primary_scaling_mock_cases() -> Vec<PrimaryMockCase> {
        vec![
            PrimaryMockCase {
                label: "500 signers, all sign (scaling)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(500)
                        .expect("generate_test_data_all_sign(500) failed")
                }),
                max_signers: 500,
                k: K,
            },
            PrimaryMockCase {
                label: "700 signers, all sign (scaling)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(700)
                        .expect("generate_test_data_all_sign(700) failed")
                }),
                max_signers: 700,
                k: K,
            },
            PrimaryMockCase {
                label: "1000 signers, all sign (scaling)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(1000)
                        .expect("generate_test_data_all_sign(1000) failed")
                }),
                max_signers: 1000,
                k: K,
            },
            PrimaryMockCase {
                label: "2000 signers, all sign (scaling)",
                make_test_data: Box::new(|| {
                    bridge_test_data_gen::generator::generate_test_data_all_sign(2000)
                        .expect("generate_test_data_all_sign(2000) failed")
                }),
                max_signers: 2000,
                k: K,
            },
        ]
    }

    /// Drive a single primary-circuit mock case end-to-end:
    /// off-circuit BLS check → build circuit → MockProver run + assert.
    fn run_primary_mock_case(case: &PrimaryMockCase) {
        let t_total = Instant::now();
        let test_data = (case.make_test_data)();

        {
            let mut keys: Vec<u16> = test_data.bk_set.keys().cloned().collect();
            keys.sort();
            println!(
                "Loaded test data: attestation={} bytes, bk_set keys={:?}",
                test_data.attestation_bytes.len(),
                keys
            );
        }

        // Off-circuit BLS verification (cheap; catches broken test-data early).
        let t = Instant::now();
        let sig_bytes = parse_signature_bytes(&test_data.attestation_bytes);
        let entries = parse_signer_entries(&test_data.attestation_bytes);
        let msg_bytes = parse_attestation_data_bytes(&test_data.attestation_bytes);
        let signature = deserialize_g2_signature(sig_bytes);
        let msg_hash = compute_msg_hash(msg_bytes);
        let pubkeys_with_counts = resolve_pubkeys(&entries, &test_data.bk_set);
        let agg_pk = compute_agg_pubkey(&pubkeys_with_counts);
        assert!(
            verify_bls_native(&signature, &agg_pk, &msg_hash),
            "Off-circuit BLS verification failed [{}]",
            case.label
        );
        println!("[timing] off-circuit BLS verification: {:?}", t.elapsed());

        let (last_seen_block_seqno, instances) = expected_public_instances(
            &test_data.attestation_bytes,
            &test_data.bk_set,
            case.max_signers,
        );
        println!(
            "block_seq_no = {}, last_seen = {}",
            last_seen_block_seqno + 1,
            last_seen_block_seqno
        );

        let lookup_bits = case.k as usize - 1;
        let t = Instant::now();
        let circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            test_data.bk_set,
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

    /// Table-driven mock-prover sanity check across primary-circuit test cases.
    /// Add new should-pass cases to `primary_mock_cases()` rather than writing
    /// a new `#[test]` function.
    #[test]
    fn test_primary_attestation_bls_checker_mock() {
        for case in primary_mock_cases() {
            println!("\n========== {} ==========", case.label);
            run_primary_mock_case(&case);
        }
    }

    /// Heavy scaling sweep (500 / 700 / 1000 / 2000 signers). Marked `#[ignore]`
    /// because it takes ~7+ minutes and several GB of RAM.
    /// Run with: `cargo test -p attestation-bls-checker-circuit
    ///   test_primary_attestation_bls_checker_mock_scaling -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn test_primary_attestation_bls_checker_mock_scaling() {
        for case in primary_scaling_mock_cases() {
            println!("\n========== {} ==========", case.label);
            run_primary_mock_case(&case);
        }
    }

    /// Primary with only 50%+1 signers should FAIL (needs 2/3). Stays separate
    /// because `#[should_panic]` is per-`#[test]`.
    #[test]
    #[should_panic]
    fn test_attestation_bls_checker_insufficient_signers_fails() {
        let test_data =
            bridge_test_data_gen::generator::generate_test_data_primary_below_threshold(10)
                .expect("generate_test_data_primary_below_threshold failed");

        let (last_seen_block_seqno, instances) = expected_public_instances(
            &test_data.attestation_bytes,
            &test_data.bk_set,
            MAX_SIGNERS,
        );

        let circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
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
        // This should panic: 6 signers doesn't meet 2/3 of 10.
        prover.assert_satisfied();
    }
}
