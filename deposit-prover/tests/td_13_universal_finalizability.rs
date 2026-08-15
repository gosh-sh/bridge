//! TD-13 — universal finalizability: supported L1 deposit envelope vs prove path.
//!
//! Catalog `TD-13`: matrix in `audit/reports/td-13-finalizability-matrix.md`.

use std::sync::Arc;

use cita_trie::{MemoryDB, PatriciaTrie, Trie};
use deposit_prover::{
    circuit_v2::{MAX_BLOCK_HEADER_BYTES, RECEIPT_PF_MAX_DEPTH},
    ethereum_fetcher::get_deposit_event_signature,
    mpt::{align_block_header_roots, receipt_proof_from_receipt},
    production_capacity_config,
    rlp_utils::EIP1559_TX_TYPE,
    synthetic_deposit_proof_input,
    synthetic_fixture::SYNTHETIC_CHAIN_ID,
    test_circuit_mock,
    types::{DepositEventData, DepositProofInput, TransactionProof},
};
use ethers::types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64};
use hasher::HasherKeccak;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn expect_finalizable(input: DepositProofInput) {
    test_circuit_mock(input, &production_capacity_config())
        .expect("TD-13: supported envelope must satisfy MockProver");
}

fn expect_not_finalizable(input: DepositProofInput) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test_circuit_mock(input, &production_capacity_config())
    }));
    match result {
        Ok(Err(_)) | Err(_) => {},
        Ok(Ok(())) => panic!("TD-13: unsupported envelope must not satisfy"),
    }
}

fn minimal_eip1559_wire() -> Vec<u8> {
    synthetic_deposit_proof_input(0).tx_proof.tx_bytes
}

fn tx_proof_unchecked(tx_bytes: Vec<u8>) -> TransactionProof {
    use axiom_eth::providers::transaction::get_tx_key_from_index;

    let memdb = Arc::new(MemoryDB::new(true));
    let hasher = Arc::new(HasherKeccak::new());
    let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));
    let key = get_tx_key_from_index(0);
    trie.insert(key.clone(), tx_bytes.clone()).expect("insert tx");
    let transactions_root: [u8; 32] = trie
        .root()
        .expect("root")
        .as_slice()
        .try_into()
        .expect("32-byte root");
    let proof_nodes = trie.get_proof(&key).expect("proof");
    TransactionProof {
        tx_bytes,
        proof_nodes,
        transactions_root,
    }
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

fn multi_log_input(logs: Vec<Log>, log_index: usize, deposit_id: u64) -> DepositProofInput {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_700u64;

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
    let tx_proof = tx_proof_unchecked(minimal_eip1559_wire());
    let mut receipt_proof = receipt_proof;
    align_block_header_roots(&mut receipt_proof, tx_proof.transactions_root).expect("align");

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

fn wire_with_type_prefix(tx_type: u8) -> Vec<u8> {
    let full = minimal_eip1559_wire();
    assert_eq!(full[0], EIP1559_TX_TYPE);
    let mut wire = vec![tx_type];
    wire.extend_from_slice(&full[1..]);
    wire
}

fn input_with_tx_wire(tx_wire: Vec<u8>) -> DepositProofInput {
    let mut input = synthetic_deposit_proof_input(5);
    input.tx_proof = tx_proof_unchecked(tx_wire);
    align_block_header_roots(&mut input.receipt_proof, input.tx_proof.transactions_root)
        .expect("align");
    input
}

#[test]
fn td_13_proof_00_supported_finalizable() {
    let input = sepolia_proof_00();
    assert_eq!(input.event_data.log_index, 2);
    assert_eq!(input.tx_proof.tx_bytes.first(), Some(&EIP1559_TX_TYPE));
    expect_finalizable(input);
}

#[test]
fn td_13_synthetic_single_log_eip1559_finalizable() {
    expect_finalizable(synthetic_deposit_proof_input(4));
}

#[test]
fn td_13_three_log_boundary_finalizable() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_800u64;

    let logs = vec![
        spam_log(contract),
        spam_log(contract),
        deposit_log(7, sender, contract, amount, an_account, timestamp),
    ];
    expect_finalizable(multi_log_input(logs, 2, 7));
}

#[test]
fn td_13_four_logs_still_not_finalizable_regression() {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = 64;
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_900u64;

    let logs = vec![
        spam_log(contract),
        spam_log(contract),
        spam_log(contract),
        deposit_log(9, sender, contract, amount, an_account, timestamp),
    ];
    expect_not_finalizable(multi_log_input(logs, 3, 9));
}

#[test]
fn td_13_non_1559_still_not_finalizable_regression() {
    expect_not_finalizable(input_with_tx_wire(wire_with_type_prefix(0x01)));
}

#[test]
fn td_13_supported_envelope_constants_match_matrix() {
  use deposit_prover::circuit_v2::{
      MAX_DATA_BYTE_LEN, MAX_LOG_NUM, RECEIPT_PF_MAX_DEPTH,
  };

    assert_eq!(MAX_LOG_NUM, 3);
    assert_eq!(MAX_DATA_BYTE_LEN, 128);
    assert_eq!(RECEIPT_PF_MAX_DEPTH, 10);
    assert_eq!(MAX_BLOCK_HEADER_BYTES, 717);
}
