//! TD-63 — `ethereum_fetcher::parse_deposit_event` edge cases (topic0, data width).

use deposit_prover::{
    ethereum_fetcher::{EthereumFetcher, get_deposit_event_signature},
    mpt::{align_block_header_roots, receipt_proof_from_receipt, transaction_proof_from_wire_bytes},
    production_capacity_config,
    synthetic_fixture::SYNTHETIC_CHAIN_ID,
    test_circuit_mock,
    types::{DepositEventData, DepositProofInput},
};
use ethers::{
    types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64},
    utils::keccak256,
};

fn fetcher() -> EthereumFetcher {
    EthereumFetcher::new("http://127.0.0.1:8545").expect("local fetcher stub")
}

fn topic_address(addr: [u8; 20]) -> H256 {
    let mut topic = [0u8; 32];
    topic[12..].copy_from_slice(&addr);
    H256::from(topic)
}

fn deposit_log_data(
    deposit_id: u64,
    sender: [u8; 20],
    contract: [u8; 20],
    amount: [u8; 32],
    an_account: [u8; 32],
    timestamp: u64,
    data_len: usize,
) -> Log {
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
            topic_address(sender),
        ],
        data: Bytes::from(data),
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

fn receipt_with_log(log: Log) -> TransactionReceipt {
    TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::zero(),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(42)),
        from: Address::from_slice(&[0x11u8; 20]),
        to: Some(Address::from_slice(&[0x22u8; 20])),
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
    }
}

fn sample_fields() -> ([u8; 20], [u8; 20], [u8; 32], [u8; 32], u64) {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_700u64;
    (sender, contract, amount, an_account, timestamp)
}

fn build_input_from_receipt(receipt: &TransactionReceipt, log_index: usize, deposit_id: u64) -> DepositProofInput {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let receipt_proof = receipt_proof_from_receipt(receipt).expect("receipt trie");
    let tx_proof = transaction_proof_from_wire_bytes(minimal_eip1559_tx_bytes(), 0)
        .expect("tx trie proof");
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root).expect("align roots");

    DepositProofInput {
        event_data: DepositEventData {
            block_number: 42,
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
fn td_63_a_valid_canonical_log_parses_ok() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let log = deposit_log_data(7, sender, contract, amount, an_account, timestamp, 128);
    let receipt = receipt_with_log(log);
    let contract_addr = Address::from_slice(&contract);

    let parsed = fetcher()
        .parse_deposit_event(&receipt, contract_addr, 0)
        .expect("TD-63 (a): canonical log");

    assert_eq!(parsed.deposit_id, 7);
    assert_eq!(parsed.sender, sender);
    assert_eq!(parsed.amount, amount);
    assert_eq!(parsed.an_workchain, 0);
    assert_eq!(parsed.an_account, an_account);
    assert_eq!(parsed.timestamp, timestamp);
    assert_eq!(parsed.block_number, 42);
}

#[test]
fn td_63_b_data_127_bytes_too_short() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let log = deposit_log_data(1, sender, contract, amount, an_account, timestamp, 127);
    let receipt = receipt_with_log(log);
    let err = fetcher()
        .parse_deposit_event(&receipt, Address::from_slice(&contract), 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("too short") || err.contains("127"),
        "TD-63 (b): {err}"
    );
}

#[test]
fn td_63_c_data_129_bytes_parse_ok_extra_ignored() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let log = deposit_log_data(2, sender, contract, amount, an_account, timestamp, 129);
    let receipt = receipt_with_log(log);
    let parsed = fetcher()
        .parse_deposit_event(&receipt, Address::from_slice(&contract), 0)
        .expect("TD-63 (c): parse accepts >=128B");
    assert_eq!(parsed.deposit_id, 2);
    assert_eq!(parsed.timestamp, timestamp);
}

#[test]
fn td_63_c_data_129_bytes_circuit_rejects_qc_td11() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let log = deposit_log_data(3, sender, contract, amount, an_account, timestamp, 129);
    let receipt = receipt_with_log(log);
    let input = build_input_from_receipt(&receipt, 0, 3);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test_circuit_mock(input, &production_capacity_config())
    }));
    match result {
        Ok(Err(err)) => assert!(
            err.contains("not satisfied") || err.contains("MockProver"),
            "TD-63 QC: circuit must reject 129B log data: {err}"
        ),
        Err(_) => {},
        Ok(Ok(())) => panic!("TD-63 QC: 129B receipt log must not satisfy circuit (TD-11)"),
    }
}

#[test]
fn td_63_d_wrong_topic0_signature_mismatch() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let mut log = deposit_log_data(4, sender, contract, amount, an_account, timestamp, 128);
    log.topics[0] = H256::from(keccak256("NotDeposit(uint256,address,uint256,int8,bytes32,uint256)"));
    let receipt = receipt_with_log(log);
    let err = fetcher()
        .parse_deposit_event(&receipt, Address::from_slice(&contract), 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("signature") || err.contains("Deposit event"),
        "TD-63 (d): {err}"
    );
}

#[test]
fn td_63_e_insufficient_topics_rejects() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let mut log = deposit_log_data(5, sender, contract, amount, an_account, timestamp, 128);
    log.topics = vec![
        H256::from(get_deposit_event_signature()),
        H256::from_low_u64_be(5),
    ];
    let receipt = receipt_with_log(log);
    let err = fetcher()
        .parse_deposit_event(&receipt, Address::from_slice(&contract), 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Not enough topics") || err.contains("topics"),
        "TD-63 (e): {err}"
    );
}

#[test]
fn td_63_f_wrong_log_address_rejects() {
    let (sender, contract, amount, an_account, timestamp) = sample_fields();
    let log = deposit_log_data(6, sender, contract, amount, an_account, timestamp, 128);
    let receipt = receipt_with_log(log);
    let wrong = Address::from_slice(&[0x99u8; 20]);
    let err = fetcher()
        .parse_deposit_event(&receipt, wrong, 0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("doesn't match expected contract") || err.contains("Log address"),
        "TD-63 (f): {err}"
    );
}
