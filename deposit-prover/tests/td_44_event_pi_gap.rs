//! TD-44 — event `timestamp` (data word 3) and `anWorkchain` (word 1) not in PI;
//! header `timestamp` (RLP slot 11) binds only via `blockHash` keccak.

use deposit_prover::{
    audit_circuit_config,
    circuit_v2::DEPOSIT_PUBLIC_INPUT_LAYOUT,
    encode_block_header,
    production_capacity_config,
    synthetic_fixture::{
        synthetic_deposit_proof_input_with_workchain, SYNTHETIC_CHAIN_ID,
    },
    test_circuit_mock,
    test_circuit_mock_instances,
    types::DepositProofInput,
    verify_block_header_rlp,
};
use ethers::types::{Address, Bloom, Block, Bytes, H256, Log, TransactionReceipt, U256, U64};
use ethers::utils::keccak256;
use halo2_base::{
    halo2_proofs::halo2curves::bn256::Fr,
    utils::ScalarField,
};

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn baseline_instances() -> Vec<Vec<Fr>> {
    test_circuit_mock_instances(sepolia_proof_00(), &audit_circuit_config(), None)
        .expect("TD-44 proof_00 baseline")
}

fn instances_operand_bytes(instances: &[Vec<Fr>]) -> Vec<u8> {
    let mut out = Vec::new();
    for fr in &instances[0] {
        out.extend_from_slice(&fr.to_bytes_le());
    }
    out
}

const EVENT_BOUND_PI_SLOTS: usize = 9; // slots 0–8: event/MPT-bound; not event timestamp or WC

fn assert_event_bound_pi_identical(baseline: &[Vec<Fr>], mutated: &[Vec<Fr>], label: &str) {
    assert_eq!(
        baseline[0][0..EVENT_BOUND_PI_SLOTS],
        mutated[0][0..EVENT_BOUND_PI_SLOTS],
        "TD-44: {label} — event-bound PI slots 0–8 must match"
    );
}

fn assert_pi_byte_identical_to_baseline(mutated: &[Vec<Fr>], label: &str) {
    let baseline = baseline_instances();
    assert_eq!(
        instances_operand_bytes(&baseline),
        instances_operand_bytes(mutated),
        "TD-44: {label} — PI operand must match proof_00 baseline"
    );
    assert_eq!(&baseline[0], &mutated[0], "TD-44: {label} — Fr instances must match");
}

fn expect_mock_reject(input: DepositProofInput) {
    let err = test_circuit_mock(input, &audit_circuit_config()).unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-44: expected circuit reject, got: {err}"
    );
}

/// Rebuild synthetic witness with explicit log-data `timestamp` word (bytes 96–127).
fn synthetic_with_timestamp_and_workchain(
    deposit_id: u64,
    an_workchain: i8,
    timestamp: u64,
) -> DepositProofInput {
    use deposit_prover::{
        ethereum_fetcher::get_deposit_event_signature,
        mpt::{align_block_header_roots, receipt_proof_from_receipt},
        types::DepositEventData,
    };

    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = ((deposit_id + 1) * 1_000).min(255) as u8;
    if amount[31] == 0 {
        amount[31] = 1;
    }
    let an_account = [0x33u8; 32];

    let mut sender_topic = [0u8; 32];
    sender_topic[12..32].copy_from_slice(&sender);

    let mut data = [0u8; 128];
    data[0..32].copy_from_slice(&amount);
    data[63] = an_workchain as u8;
    data[64..96].copy_from_slice(&an_account);
    let mut ts_word = [0u8; 32];
    U256::from(timestamp).to_big_endian(&mut ts_word);
    data[96..128].copy_from_slice(&ts_word);

    let log = Log {
        address: Address::from_slice(&contract),
        topics: vec![
            H256::from(get_deposit_event_signature()),
            H256::from_low_u64_be(deposit_id),
            H256(sender_topic),
        ],
        data: Bytes::from(data.to_vec()),
        ..Default::default()
    };

    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::zero(),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(1)),
        from: Address::from_slice(&sender),
        to: Some(Address::from_slice(&contract)),
        cumulative_gas_used: U256::from(21_000),
        gas_used: Some(U256::from(21_000)),
        contract_address: None,
        logs: vec![log],
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: None,
        effective_gas_price: None,
        other: Default::default(),
    };

    let template = synthetic_deposit_proof_input_with_workchain(deposit_id, an_workchain);
    let tx_proof = template.tx_proof.clone();

    let receipt_proof = receipt_proof_from_receipt(&receipt)
        .expect("synthetic receipt trie proof must build");
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root)
        .expect("block header roots must align");

    let event_data = DepositEventData {
        block_number: 1,
        transaction_index: 0,
        log_index: 0,
        deposit_id,
        sender,
        amount,
        an_workchain,
        an_account,
        timestamp,
        contract_address: contract,
        chain_id: SYNTHETIC_CHAIN_ID,
    };

    DepositProofInput {
        event_data,
        receipt_proof,
        tx_proof,
        dapp_id: [0u8; 32],
    }
}

#[test]
fn td_44_public_input_layout_has_no_event_timestamp_or_workchain_slots() {
    assert!(!DEPOSIT_PUBLIC_INPUT_LAYOUT.iter().any(|label| {
        label.eq_ignore_ascii_case("timestamp")
            || label.eq_ignore_ascii_case("anWorkchain")
            || label.eq_ignore_ascii_case("workchain")
    }));
}

#[test]
fn td_44_proof_00_baseline_mock_passes() {
    test_circuit_mock(sepolia_proof_00(), &audit_circuit_config())
        .expect("TD-44 proof_00 baseline MockProver");
    let inst = baseline_instances();
    assert_eq!(inst[0].len(), 12);
}

#[test]
fn td_44_proof_00_event_data_timestamp_drift_pi_unchanged() {
    let mut input = sepolia_proof_00();
    input.event_data.timestamp = 1;
    let inst =
        test_circuit_mock_instances(input, &audit_circuit_config(), None).expect("event_data ts drift");
    assert_pi_byte_identical_to_baseline(&inst, "event_data.timestamp only");
}

#[test]
fn td_44_mutated_log_timestamp_word_mock_passes_pi_identical() {
    let baseline = synthetic_with_timestamp_and_workchain(44, 0, 1_700_000_000);
    let baseline_pi =
        test_circuit_mock_instances(baseline.clone(), &production_capacity_config(), None)
            .expect("baseline synthetic ts");

    for ts in [0u64, 1, 1_700_000_999, u64::MAX] {
        let mutated = synthetic_with_timestamp_and_workchain(44, 0, ts);
        let inst =
            test_circuit_mock_instances(mutated, &production_capacity_config(), None)
                .unwrap_or_else(|e| panic!("TD-44 log ts={ts}: unexpected reject: {e}"));
        assert_event_bound_pi_identical(
            &baseline_pi,
            &inst,
            &format!("log data timestamp word ts={ts}"),
        );
    }
}

#[test]
fn td_44_mutated_an_workchain_byte_mock_passes_pi_identical() {
    let baseline = synthetic_with_timestamp_and_workchain(44, 0, 1_700_000_444);
    let baseline_pi =
        test_circuit_mock_instances(baseline, &production_capacity_config(), None)
            .expect("baseline wc=0");

    for wc in [127i8, -1i8, 42i8] {
        let mutated = synthetic_with_timestamp_and_workchain(44, wc, 9_999_999_999);
        assert_eq!(mutated.event_data.an_workchain, wc);
        let inst =
            test_circuit_mock_instances(mutated, &production_capacity_config(), None)
                .unwrap_or_else(|e| panic!("TD-44 wc={wc}: unexpected reject: {e}"));
        assert_event_bound_pi_identical(
            &baseline_pi,
            &inst,
            &format!("anWorkchain byte 63 wc={wc}"),
        );
    }
}

#[test]
fn td_44_header_timestamp_increment_fails_verify_block_header_rlp() {
    let block: Block<H256> =
        serde_json::from_str(include_str!("../fixtures/headers/sepolia_prague.json")).unwrap();
    let canonical = verify_block_header_rlp(&block).expect("canonical header");
    let mut bumped = block.clone();
    bumped.timestamp = bumped.timestamp + U256::from(1);
    let mutated = encode_block_header(&bumped).expect("encode bumped timestamp");
    assert_ne!(
        keccak256(&canonical),
        keccak256(&mutated),
        "header timestamp slot 11 must change block hash"
    );
    let err = verify_block_header_rlp(&bumped).unwrap_err().to_string();
    assert!(
        err.contains("does not reproduce"),
        "TD-44: canonical hash bind rejects timestamp drift: {err}"
    );
}

#[test]
fn td_44_corrupt_witness_header_rlp_mock_rejects_control() {
    let mut input = sepolia_proof_00();
    if !input.receipt_proof.block_header_rlp.is_empty() {
        input.receipt_proof.block_header_rlp[0] ^= 0x01;
    }
    expect_mock_reject(input);
}
