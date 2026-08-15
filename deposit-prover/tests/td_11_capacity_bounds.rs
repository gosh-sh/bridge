//! TD-11 — circuit capacity bounds (`MAX_LOG_NUM`, 128B data, MPT depth, header).
//!
//! Uses [`production_capacity_config`] (VK limits), not audit's relaxed `max_log_num=20`.

use deposit_prover::{
    circuit_v2::{MAX_BLOCK_HEADER_BYTES, RECEIPT_PF_MAX_DEPTH},
    ethereum_fetcher::get_deposit_event_signature,
    mpt::{align_block_header_roots, receipt_proof_from_receipt, transaction_proof_from_wire_bytes},
    production_capacity_config,
    synthetic_fixture::SYNTHETIC_CHAIN_ID,
    test_circuit_mock,
    types::{DepositEventData, DepositProofInput},
};
use ethers::types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64};

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_capacity_rejected(input: DepositProofInput) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test_circuit_mock(input, &production_capacity_config())
    }));
    match result {
        Ok(Err(err)) => assert!(
            err.contains("not satisfied")
                || err.contains("MockProver")
                || err.contains("exceeds MAX_BLOCK_HEADER_BYTES"),
            "TD-11: expected reject, got: {err}"
        ),
        Err(_) => {}, // axiom-eth / circuit assert — fail-closed
        Ok(Ok(())) => panic!("TD-11: oversized witness must not satisfy"),
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

fn spam_log(token: [u8; 20]) -> Log {
    Log {
        address: Address::from_slice(&token),
        topics: vec![H256::from_low_u64_be(0xdead)],
        data: Bytes::from(vec![0x01]),
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
    data_len: usize,
) -> Log {
    let mut sender_topic = [0u8; 32];
    sender_topic[12..32].copy_from_slice(&sender);

    let mut data = vec![0u8; data_len];
    if data_len >= 32 {
        data[0..32].copy_from_slice(&amount);
    }
    if data_len >= 64 {
        data[63] = 0;
        data[64..96].copy_from_slice(&an_account);
    }
    if data_len >= 128 {
        let mut ts_word = [0u8; 32];
        U256::from(timestamp).to_big_endian(&mut ts_word);
        data[96..128].copy_from_slice(&ts_word);
    }

    Log {
        address: Address::from_slice(&contract),
        topics: vec![
            H256::from(get_deposit_event_signature()),
            H256::from_low_u64_be(deposit_id),
            H256(sender_topic),
        ],
        data: Bytes::from(data),
        ..Default::default()
    }
}

fn build_input(logs: Vec<Log>, log_index: usize, deposit_id: u64) -> DepositProofInput {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_300u64;

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

    let receipt_proof = receipt_proof_from_receipt(&receipt).expect("receipt trie");
    let tx_proof = transaction_proof_from_wire_bytes(minimal_eip1559_tx_bytes(), 0)
        .expect("tx trie proof");
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root)
        .expect("align roots");

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

#[test]
fn td_11_proof_00_passes_under_production_capacity_config() {
    let input = sepolia_proof_00();
    assert_eq!(input.event_data.log_index, 2);
    test_circuit_mock(input, &production_capacity_config()).unwrap();
}

#[test]
fn td_11_three_logs_boundary_passes() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_400u64;

    let logs = vec![
        spam_log(contract),
        spam_log(contract),
        deposit_log(7, sender, contract, amount, an_account, timestamp, 128),
    ];
    let input = build_input(logs, 2, 7);
    test_circuit_mock(input, &production_capacity_config()).unwrap();
}

#[test]
fn td_11_four_logs_exceeds_max_log_num_rejected() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_500u64;

    let logs = vec![
        spam_log(contract),
        spam_log(contract),
        spam_log(contract),
        deposit_log(9, sender, contract, amount, an_account, timestamp, 128),
    ];
    let input = build_input(logs, 3, 9);
    expect_capacity_rejected(input);
}

#[test]
fn td_11_event_data_over_128_bytes_rejected() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_600u64;

    let logs = vec![deposit_log(1, sender, contract, amount, an_account, timestamp, 129)];
    let input = build_input(logs, 0, 1);
    expect_capacity_rejected(input);
}

#[test]
fn td_11_mpt_depth_eleven_rejected() {
    let mut input = sepolia_proof_00();
    let pad_node = vec![0x80u8];
    while input.receipt_proof.proof_nodes.len() <= RECEIPT_PF_MAX_DEPTH {
        input
            .receipt_proof
            .proof_nodes
            .insert(0, pad_node.clone());
    }
    assert!(
        input.receipt_proof.proof_nodes.len() > RECEIPT_PF_MAX_DEPTH,
        "TD-11: need depth > {}",
        RECEIPT_PF_MAX_DEPTH
    );
    expect_capacity_rejected(input);
}

#[test]
fn td_11_header_over_max_bytes_rejected() {
    let mut input = sepolia_proof_00();
    if input.receipt_proof.block_header_rlp.len() <= MAX_BLOCK_HEADER_BYTES {
        input
            .receipt_proof
            .block_header_rlp
            .resize(MAX_BLOCK_HEADER_BYTES + 1, 0x00);
    }
    assert!(
        input.receipt_proof.block_header_rlp.len() > MAX_BLOCK_HEADER_BYTES,
        "header must exceed cap"
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test_circuit_mock(input, &production_capacity_config())
    }));
    match result {
        Ok(Err(err)) => assert!(
            err.contains("not satisfied") || err.contains("exceeds MAX_BLOCK_HEADER_BYTES"),
            "TD-11 header: {err}"
        ),
        Err(_) => {},
        Ok(Ok(())) => panic!("TD-11: oversized header must not satisfy"),
    }
}
