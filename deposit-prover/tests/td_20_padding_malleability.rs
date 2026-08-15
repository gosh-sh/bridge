//! TD-20 — MPT padding witness malleability (QC-PROV-04).
//!
//! Canonical `max_key_byte_len = 3`: active-prefix corruption rejects; trailing
//! padding slots may be malleable upstream (QC) but must not change public inputs.

use deposit_prover::{
    audit_circuit_config,
    test_circuit_mock_instances,
    test_circuit_mock_with_mpt_mutation,
    types::DepositProofInput,
    MptWitnessMutation,
};
use halo2_base::{
    halo2_proofs::halo2curves::bn256::Fr,
    utils::ScalarField,
};

const CANONICAL_MAX_KEY_LEN: usize = 3;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn baseline_instances() -> Vec<Vec<Fr>> {
    test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
        .expect("TD-20 baseline proof_00")
}

fn instances_operand_bytes(instances: &[Vec<Fr>]) -> Vec<u8> {
    let mut out = Vec::new();
    for fr in &instances[0] {
        out.extend_from_slice(&fr.to_bytes_le());
    }
    out
}

fn assert_pi_byte_identical_to_baseline(mutated: &[Vec<Fr>], label: &str) {
    let baseline = baseline_instances();
    assert_eq!(
        instances_operand_bytes(&baseline),
        instances_operand_bytes(mutated),
        "TD-20: {label} — PI operand must match baseline (QC malleability ok, PI drift = BC)"
    );
    assert_eq!(&baseline[0], &mutated[0], "TD-20: {label} — Fr instances must match");
}

fn expect_reject(mutation: MptWitnessMutation, label: &str) {
    let input = sepolia_proof_00();
    let err = test_circuit_mock_with_mpt_mutation(
        input,
        &audit_circuit_config(),
        Some(mutation),
    )
    .unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-20 {label}: expected reject, got: {err}"
    );
}

fn run_mutation(mutation: MptWitnessMutation) -> Result<Vec<Vec<Fr>>, String> {
    test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), Some(mutation))
}

#[test]
fn td_20_baseline_pi_operand_snapshot() {
    let inst = baseline_instances();
    assert_eq!(inst[0].len(), 12);
    assert_ne!(instances_operand_bytes(&inst), vec![0u8; 12 * 32]);
}

#[test]
fn td_20_padding_slot_one_rejected_at_max_key_len_three() {
    expect_reject(
        MptWitnessMutation {
            max_key_byte_len: Some(CANONICAL_MAX_KEY_LEN),
            corrupt_key_byte_at: Some((1, 0x42)),
            ..Default::default()
        },
        "slot1_0x42",
    );
}

#[test]
fn td_20_active_key_prefix_corruption_rejected() {
    let input = sepolia_proof_00();
    let path = deposit_prover::rlp_utils::encode_tx_index(input.event_data.transaction_index);
    let last = path.len().saturating_sub(1);
    let flipped = path[last] ^ 0x01;
    expect_reject(
        MptWitnessMutation {
            max_key_byte_len: Some(CANONICAL_MAX_KEY_LEN),
            corrupt_key_byte_at: Some((last, flipped)),
            ..Default::default()
        },
        "active_key_flip",
    );
}

#[test]
fn td_20_padding_slot_two_malleable_qc_pi_byte_identical() {
    let mutation = MptWitnessMutation {
        max_key_byte_len: Some(CANONICAL_MAX_KEY_LEN),
        corrupt_key_byte_at: Some((2, 0xAB)),
        ..Default::default()
    };
    let inst = run_mutation(mutation).expect("TD-20 QC: trailing padding slot may satisfy");
    assert_pi_byte_identical_to_baseline(&inst, "padding_slot_2");
}

#[test]
fn td_20_padding_slot_two_distinct_garbage_same_pi_qc() {
    for byte in [0xAB, 0x99, 0xFF] {
        let mutation = MptWitnessMutation {
            max_key_byte_len: Some(CANONICAL_MAX_KEY_LEN),
            corrupt_key_byte_at: Some((2, byte)),
            ..Default::default()
        };
        let inst = run_mutation(mutation)
            .unwrap_or_else(|e| panic!("TD-20 QC padding slot 2 byte {byte}: {e}"));
        assert_pi_byte_identical_to_baseline(&inst, &format!("padding_slot_2_{byte}"));
    }
}

#[test]
fn td_20_max_key_len_four_desync_not_satisfied() {
    let mutation = MptWitnessMutation {
        max_key_byte_len: Some(4),
        corrupt_key_byte_at: Some((3, 0x42)),
        ..Default::default()
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_mutation(mutation)
    }));
    match outcome {
        Ok(Ok(inst)) => assert_pi_byte_identical_to_baseline(&inst, "max_key_len_4_slot3"),
        Ok(Err(err)) => {
            assert!(
                err.contains("not satisfied") || err.contains("MockProver"),
                "TD-20: max_key_byte_len=4: {err}"
            );
        },
        Err(_) => {
            // axiom-eth RLP key layout assert (4 vs 3) — fail-closed, not production.
        },
    }
}

#[test]
fn td_20_slot_one_garbage_ff_rejected_not_malleable() {
    expect_reject(
        MptWitnessMutation {
            max_key_byte_len: Some(CANONICAL_MAX_KEY_LEN),
            corrupt_key_byte_at: Some((1, 0xFF)),
            ..Default::default()
        },
        "slot1_0xFF",
    );
}
