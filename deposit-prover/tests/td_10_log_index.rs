//! TD-10 — multi-log receipt / wrong `log_index` witness (deposit-prover).
//!
//! Catalog `TD-10`: receipt with ≥3 logs (deposit not at index 0); wrong witness
//! `log_index` must fail MockProver — no mint from Transfer / wrong deposit slot.

use deposit_prover::{
    audit_circuit_config,
    ethereum_fetcher::get_deposit_event_signature,
    mpt::{align_block_header_roots, receipt_proof_from_receipt, transaction_proof_from_wire_bytes},
    synthetic_fixture::SYNTHETIC_CHAIN_ID,
    test_circuit_mock,
    test_circuit_mock_instances,
    types::{DepositEventData, DepositProofInput},
};
use ethers::types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64};
use ethers_core::utils::keccak256;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

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

fn address_to_fr(addr: &[u8; 20]) -> Fr {
    let mut padded = [0u8; 32];
    padded[12..].copy_from_slice(addr);
    horner_fr(&padded)
}

fn with_log_index(input: &DepositProofInput, log_index: usize) -> DepositProofInput {
    let mut cloned = input.clone();
    cloned.event_data.log_index = log_index;
    cloned
}

fn expect_wrong_log_index_rejected(input: DepositProofInput) {
    let err = test_circuit_mock(input, &audit_circuit_config()).unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-10: expected circuit reject, got: {err}"
    );
}

fn topic_address(addr: [u8; 20]) -> H256 {
    let mut topic = [0u8; 32];
    topic[12..].copy_from_slice(&addr);
    H256::from(topic)
}

fn erc20_transfer_log(
    token: [u8; 20],
    from: [u8; 20],
    to: [u8; 20],
    amount: [u8; 32],
) -> Log {
    let sig = H256::from(keccak256("Transfer(address,address,uint256)"));
    Log {
        address: Address::from_slice(&token),
        topics: vec![sig, topic_address(from), topic_address(to)],
        data: Bytes::from(amount.to_vec()),
        ..Default::default()
    }
}

fn erc20_approval_log(
    token: [u8; 20],
    owner: [u8; 20],
    spender: [u8; 20],
    amount: [u8; 32],
) -> Log {
    let sig = H256::from(keccak256("Approval(address,address,uint256)"));
    Log {
        address: Address::from_slice(&token),
        topics: vec![sig, topic_address(owner), topic_address(spender)],
        data: Bytes::from(amount.to_vec()),
        ..Default::default()
    }
}

fn deposit_log(
    deposit_id: u64,
    sender: [u8; 20],
    contract: [u8; 20],
    amount: [u8; 32],
    an_account: [u8; 32],
    timestamp: u64,
) -> Log {
    let mut sender_topic = [0u8; 32];
    sender_topic[12..32].copy_from_slice(&sender);

    let mut data = [0u8; 128];
    data[0..32].copy_from_slice(&amount);
    data[63] = 0;
    data[64..96].copy_from_slice(&an_account);
    let mut ts_word = [0u8; 32];
    U256::from(timestamp).to_big_endian(&mut ts_word);
    data[96..128].copy_from_slice(&ts_word);

    Log {
        address: Address::from_slice(&contract),
        topics: vec![
            H256::from(get_deposit_event_signature()),
            H256::from_low_u64_be(deposit_id),
            H256(sender_topic),
        ],
        data: Bytes::from(data.to_vec()),
        ..Default::default()
    }
}

fn minimal_eip1559_tx_bytes() -> Vec<u8> {
    use alloy_rlp::Encodable;

    let mut tx_payload = Vec::new();
    SYNTHETIC_CHAIN_ID.encode(&mut tx_payload);
    0u64.encode(&mut tx_payload);
    0u64.encode(&mut tx_payload);
    0u64.encode(&mut tx_payload);
    21_000u64.encode(&mut tx_payload);
    vec![0u8; 20].encode(&mut tx_payload);
    0u64.encode(&mut tx_payload);
    Vec::<u8>::new().encode(&mut tx_payload);
    {
        let mut al = Vec::new();
        alloy_rlp::Header {
            list: true,
            payload_length: 0,
        }
        .encode(&mut al);
        tx_payload.extend_from_slice(&al);
    }
    0u64.encode(&mut tx_payload);
    vec![0u8; 32].encode(&mut tx_payload);
    vec![0u8; 32].encode(&mut tx_payload);
    let mut tx_list = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: tx_payload.len(),
    }
    .encode(&mut tx_list);
    tx_list.extend_from_slice(&tx_payload);
    let mut tx_bytes = vec![0x02u8];
    tx_bytes.extend_from_slice(&tx_list);
    tx_bytes
}

fn multi_log_deposit_input(
    logs: Vec<Log>,
    log_index: usize,
    deposit_id: u64,
    sender: [u8; 20],
    contract: [u8; 20],
    amount: [u8; 32],
    an_account: [u8; 32],
    timestamp: u64,
) -> DepositProofInput {
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
        logs,
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: None,
        effective_gas_price: None,
        other: Default::default(),
    };

    let receipt_proof = receipt_proof_from_receipt(&receipt).expect("multi-log receipt trie");
    let tx_bytes = minimal_eip1559_tx_bytes();
    let tx_proof = transaction_proof_from_wire_bytes(tx_bytes, 0).expect("tx trie proof");
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root)
        .expect("align header roots");

    DepositProofInput {
        event_data: DepositEventData {
            block_number: 1,
            transaction_index: 0,
            log_index,
            deposit_id,
            sender,
            amount,
            an_workchain: 0,
            an_account,
            timestamp,
            contract_address: contract,
            chain_id: SYNTHETIC_CHAIN_ID,
        },
        receipt_proof,
        tx_proof,
        dapp_id: [0u8; 32],
    }
}

fn synthetic_three_log_deposit_at_index_two() -> DepositProofInput {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_100u64;
    let peer = [0x44u8; 20];

    let logs = vec![
        erc20_transfer_log(contract, sender, peer, amount),
        erc20_approval_log(contract, sender, peer, amount),
        deposit_log(7, sender, contract, amount, an_account, timestamp),
    ];

    multi_log_deposit_input(
        logs,
        2,
        7,
        sender,
        contract,
        amount,
        an_account,
        timestamp,
    )
}

#[test]
fn td_10_proof_00_multi_log_happy_path_pi_matches_event() {
    let input = sepolia_proof_00();
    assert_eq!(input.event_data.log_index, 2);

    let instances =
        test_circuit_mock_instances(input.clone(), &audit_circuit_config(), None).unwrap();
    let pis = &instances[0];

    assert_eq!(pis[0], Fr::from(input.event_data.deposit_id));
    assert_eq!(pis[1], address_to_fr(&input.event_data.sender));
    assert_eq!(pis[2], horner_fr(&input.event_data.amount));
}

#[test]
fn td_10_wrong_log_index_points_at_transfer_rejected() {
    let base = sepolia_proof_00();
    expect_wrong_log_index_rejected(with_log_index(&base, 0));
}

#[test]
fn td_10_wrong_log_index_points_at_approval_rejected() {
    let base = sepolia_proof_00();
    expect_wrong_log_index_rejected(with_log_index(&base, 1));
}

#[test]
fn td_10_log_index_out_of_range_rejected() {
    let base = sepolia_proof_00();
    expect_wrong_log_index_rejected(with_log_index(&base, 99));
}

#[test]
fn td_10_wrong_log_index_same_topic_different_deposit_id_rejected() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_200u64;

    let logs = vec![
        erc20_transfer_log(contract, sender, contract, amount),
        deposit_log(3, sender, contract, amount, an_account, timestamp),
        deposit_log(7, sender, contract, amount, an_account, timestamp + 1),
    ];

    // Witness targets decoy deposit (id=3) while event claims deposit_id=7.
    let input = multi_log_deposit_input(
        logs,
        1,
        7,
        sender,
        contract,
        amount,
        an_account,
        timestamp + 1,
    );
    expect_wrong_log_index_rejected(input);
}

#[test]
fn td_10_synthetic_three_log_deposit_at_index_two_happy_path() {
    let input = synthetic_three_log_deposit_at_index_two();
    test_circuit_mock(input.clone(), &audit_circuit_config()).unwrap();
    assert_eq!(input.event_data.log_index, 2);
    assert_eq!(input.event_data.deposit_id, 7);
}
