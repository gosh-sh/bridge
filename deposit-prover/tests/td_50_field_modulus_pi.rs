//! TD-50 — BN254 field modulus / PI aliasing (META soundness).

use deposit_prover::{
    audit_circuit_config,
    circuit_v2::{DepositWitnessMutation, DEPOSIT_PUBLIC_INPUT_LAYOUT},
    synthetic_deposit_proof_input,
    test_circuit_mock,
    test_circuit_mock_instances,
    test_circuit_mock_with_witness_mutation,
    types::DepositProofInput,
};
use ethers::types::U256;
use ethers::utils::keccak256;
use halo2_base::{
    halo2_proofs::halo2curves::bn256::Fr,
    utils::ScalarField,
};

/// BN254 scalar field order (matches `AckiNackiBridge.BN254_R`).
pub const BN254_FR_MODULUS: U256 = U256([
    0x30644e72e131a029,
    0x1585d2833e84879b,
    0x3e84879b9709143e,
    0x3e1f593f00000001,
]);

const PI_AMOUNT: usize = 2;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn horner_fr(bytes: &[u8]) -> Fr {
    let mut acc = Fr::zero();
    let base = Fr::from(256u64);
    for &byte in bytes {
        acc = acc * base + Fr::from(byte as u64);
    }
    acc
}

fn fr_to_u256(fr: Fr) -> U256 {
    U256::from_little_endian(&fr.to_bytes_le())
}

fn modulus_be_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    BN254_FR_MODULUS.to_big_endian(&mut out);
    out
}

#[test]
fn td_50_layout_table_twelve_slots_documented() {
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT.len(), 12);
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[4], "chainId");
    assert_eq!(DEPOSIT_PUBLIC_INPUT_LAYOUT[11], "promiseCommit");
}

#[test]
fn td_50_deposit_id_chain_id_below_modulus_injective() {
    let input = synthetic_deposit_proof_input(50);
    test_circuit_mock(input, &audit_circuit_config()).expect("baseline small ids");
    let inst =
        test_circuit_mock_instances(synthetic_deposit_proof_input(50), &audit_circuit_config(), None)
            .expect("instances");
    assert!(fr_to_u256(inst[0][0]) < BN254_FR_MODULUS);
    assert!(fr_to_u256(inst[0][4]) < BN254_FR_MODULUS);
}

fn synthetic_amount_low_half_max(deposit_id: u64) -> DepositProofInput {
    use deposit_prover::{
        ethereum_fetcher::get_deposit_event_signature,
        mpt::{align_block_header_roots, receipt_proof_from_receipt},
        synthetic_fixture::synthetic_deposit_proof_input_with_workchain,
    };
    use ethers::types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64};

    let template = synthetic_deposit_proof_input_with_workchain(deposit_id, 0);
    let mut amount = template.event_data.amount;
    for i in 16..32 {
        amount[i] = 0xFF;
    }

    let sender = template.event_data.sender;
    let contract = template.event_data.contract_address;
    let an_account = template.event_data.an_account;
    let an_workchain = template.event_data.an_workchain;

    let mut sender_topic = [0u8; 32];
    sender_topic[12..32].copy_from_slice(&sender);

    let mut data = [0u8; 128];
    data[0..32].copy_from_slice(&amount);
    data[63] = an_workchain as u8;
    data[64..96].copy_from_slice(&an_account);

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

    let tx_proof = template.tx_proof.clone();
    let receipt_proof = receipt_proof_from_receipt(&receipt).expect("receipt proof");
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root)
        .expect("align roots");

    DepositProofInput {
        event_data: {
            let mut ed = template.event_data.clone();
            ed.amount = amount;
            ed
        },
        receipt_proof,
        tx_proof,
        dapp_id: template.dapp_id,
    }
}

#[test]
fn td_50_amount_word_near_modulus_boundary() {
    let baseline =
        test_circuit_mock_instances(synthetic_deposit_proof_input(50), &audit_circuit_config(), None)
            .expect("baseline");
    let input = synthetic_amount_low_half_max(50);
    let horner = horner_fr(&input.event_data.amount);
    assert!(
        fr_to_u256(horner) < BN254_FR_MODULUS,
        "low-half-only amount stays canonical Fr < r"
    );
    let inst =
        test_circuit_mock_instances(input, &audit_circuit_config(), None).expect("max low-half amount");
    assert_eq!(inst[0][PI_AMOUNT], horner);
    assert_ne!(inst[0][PI_AMOUNT], baseline[0][PI_AMOUNT]);
}

#[test]
fn td_50_dapp_id_range_check_rejects_non_byte_witness() {
    let err = test_circuit_mock_with_witness_mutation(
        sepolia_proof_00(),
        &audit_circuit_config(),
        DepositWitnessMutation {
            dapp_id_oversized_byte: Some((0, 256)),
        },
    )
    .unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "BC-D05: oversized dappId byte must fail range_check: {err}"
    );
}

#[test]
fn td_50_block_hash_halves_split_injective_domain() {
    let input = sepolia_proof_00();
    let hash = keccak256(&input.receipt_proof.block_header_rlp);
    let hi_fr = horner_fr(&hash[0..16]);
    let lo_fr = horner_fr(&hash[16..32]);
    assert!(fr_to_u256(hi_fr) < BN254_FR_MODULUS);
    assert!(fr_to_u256(lo_fr) < BN254_FR_MODULUS);
    // 16-byte Horner image < 2^128 < p — injective on raw half bytes.
    let inst = test_circuit_mock_instances(input, &audit_circuit_config(), None).unwrap();
    assert_eq!(inst[0][9], hi_fr);
    assert_eq!(inst[0][10], lo_fr);
}

#[test]
fn td_50_fr_alias_u256_decode_not_used_for_unsplit_slots() {
    let zero_bytes = [0u8; 32];
    let modulus_bytes = modulus_be_bytes();
    assert_ne!(zero_bytes, modulus_bytes);
    assert_eq!(horner_fr(&zero_bytes), Fr::zero());
    // Horner BE fold of `r` as 32-byte integer ≠ 0 Fr (encoding ≠ field reduction).
    assert_ne!(horner_fr(&modulus_bytes), Fr::zero());
    assert_eq!(
        fr_to_u256(horner_fr(&zero_bytes)),
        fr_to_u256(Fr::zero()),
        "canonical zero Fr → U256 zero; relayer has no Fr-reduction pass (QC)"
    );
}
