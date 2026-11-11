use super::*;

const BLOCK: &str = include_str!("fixtures/arbitrum_sepolia_326657966_block.json");
const RECEIPTS: &str = include_str!("fixtures/arbitrum_sepolia_326657966_receipts.json");
/// The same block as `scripts/deposit-receipt/vectors.py live` wrote it: an
/// independent encoder, and the vector the TVM tests prove on-chain.
const VECTOR: &str =
    include_str!("../../../scripts/deposit-receipt/fixtures/arbitrum_sepolia_326657966.json");

fn unhex(s: &str) -> Vec<u8> {
    alloy::primitives::hex::decode(s).unwrap()
}

#[test]
fn a_live_arbitrum_block_rebuilds_its_receipts_root_and_hash() {
    let header: Header = serde_json::from_str(BLOCK).unwrap();
    let mut receipts: Vec<RpcReceipt> = serde_json::from_str(RECEIPTS).unwrap();
    receipts.sort_by_key(|r| r.transaction_index);
    let types: std::collections::BTreeSet<u64> = receipts
        .iter()
        .map(|r| r.ty.map_or(0, |t| t.to()))
        .collect();
    assert_eq!(types, [0, 2, 0x6a].into());

    let values: Vec<Vec<u8>> = receipts.iter().map(RpcReceipt::trie_value).collect();
    let vector: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
    let fd = &vector["finalizeDeposit"];
    let index = fd["txIndex"].as_u64().unwrap() as usize;
    let (root, proof) = receipt_proof(&values, index);
    assert_eq!(root, header.inner.receipts_root);
    let expected: Vec<Vec<u8>> = fd["receiptProof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| unhex(p.as_str().unwrap()))
        .collect();
    assert_eq!(
        proof.iter().map(|p| p.to_vec()).collect::<Vec<_>>(),
        expected
    );

    let rlp = alloy::rlp::encode(&header.inner);
    assert_eq!(keccak256(&rlp), header.hash);
    assert_eq!(rlp, unhex(fd["header"].as_str().unwrap()));
}

fn rebuilds_root_and_hash(block: &str, receipts: &str) -> std::collections::BTreeSet<u64> {
    let header: Header = serde_json::from_str(block).unwrap();
    let mut receipts: Vec<RpcReceipt> = serde_json::from_str(receipts).unwrap();
    receipts.sort_by_key(|r| r.transaction_index);
    let values: Vec<Vec<u8>> = receipts.iter().map(RpcReceipt::trie_value).collect();
    for index in 0..values.len() {
        assert_eq!(
            receipt_proof(&values, index).0,
            header.inner.receipts_root,
            "{index}"
        );
    }
    assert_eq!(keccak256(alloy::rlp::encode(&header.inner)), header.hash);
    receipts
        .iter()
        .map(|r| r.ty.map_or(0, |t| t.to()))
        .collect()
}

#[test]
fn a_live_op_stack_block_with_canyon_deposit_receipts_rebuilds_its_root() {
    let types = rebuilds_root_and_hash(
        include_str!("fixtures/base_sepolia_55363630_block.json"),
        include_str!("fixtures/base_sepolia_55363630_receipts.json"),
    );
    assert_eq!(types, [0, 2, OP_DEPOSIT_TX_TYPE as u64].into());
}

#[test]
fn a_live_mantle_block_whose_deposit_receipt_has_no_version_rebuilds_its_root() {
    let types = rebuilds_root_and_hash(
        include_str!("fixtures/mantle_102932123_block.json"),
        include_str!("fixtures/mantle_102932123_receipts.json"),
    );
    assert_eq!(types, [0, OP_DEPOSIT_TX_TYPE as u64].into());
}

fn witness(nodes: usize, node_len: usize, header_len: usize, log_index: u16) -> DepositWitness {
    DepositWitness {
        chain_id: 1,
        block_number: 1,
        block_hash: B256::ZERO,
        header: vec![0; header_len].into(),
        tx_index: 0,
        receipt_proof: vec![vec![0; node_len].into(); nodes],
        log_index,
    }
}

/// Gas measured in tvm-debugger up to `tvm.accept()` of eccBridge 1.17.0:
/// (proof nodes, header plus proof bytes, logIndex, gas).
const MEASURED: [(usize, usize, u16, u64); 11] = [
    (3, 1_825, 0, 107_910),
    (3, 4_966, 20, 185_366),
    (3, 11_246, 60, 357_619),
    (3, 17_526, 100, 510_667),
    (3, 25_376, 150, 730_985),
    (3, 13_016, 10, 249_961),
    (3, 29_801, 25, 462_266),
    (3, 46_586, 40, 674_231),
    (7, 3_040, 0, 202_312),
    (7, 12_461, 60, 452_021),
    (7, 12_926, 60, 453_527),
];

#[test]
fn the_gas_estimate_stays_above_every_measurement_and_refuses_none_that_fits() {
    for (nodes, bytes, log_index, gas) in MEASURED {
        let w = witness(nodes, 0, bytes, log_index);
        let estimate = w.estimated_gas();
        assert!(
            estimate > gas + gas / 25,
            "{nodes} nodes, {bytes} bytes, log {log_index}: {estimate} vs {gas}"
        );
        if gas + gas / 25 <= AN_GAS_CREDIT {
            assert!(
                w.check_gas().is_ok(),
                "{nodes} nodes, {bytes} bytes, log {log_index}: refused at {estimate}, takes {gas}"
            );
        }
    }
}

#[test]
fn a_deposit_over_the_gas_credit_is_refused_and_stays_below_the_message_limit() {
    assert!(witness(3, 500, 600, 60).check_gas().is_ok());
    assert!(witness(3, 500, 600, 160).check_gas().is_err());
    let largest = (AN_GAS_CREDIT - 30_000 - 22_000) / 11;
    assert!(witness(1, 0, largest as usize, 0).check_gas().is_ok());
    assert!(witness(1, 0, largest as usize + 1, 0).check_gas().is_err());
    assert!(largest < 48 * 1024);
}

#[test]
fn only_the_bridges_six_field_deposit_passes_the_shape_check() {
    let bridge = Address::repeat_byte(0x42);
    let deposit = RpcLog {
        address: bridge,
        topics: vec![
            DEPOSIT_TOPIC0,
            B256::with_last_byte(7),
            B256::with_last_byte(0xab),
        ],
        data: vec![0u8; DEPOSIT_DATA_LEN].into(),
    };
    assert_eq!(
        check_deposit_log(&deposit, bridge),
        Ok(B256::with_last_byte(7))
    );
    assert!(check_deposit_log(&deposit, Address::repeat_byte(0x43)).is_err());

    // The five-field event the bridge emitted before the token was added.
    let legacy = keccak256("Deposit(uint256,address,uint256,bytes32,uint256)");
    let mut log = deposit.clone();
    log.topics[0] = legacy;
    log.data = vec![0u8; 96].into();
    assert!(check_deposit_log(&log, bridge).is_err());

    let mut log = deposit.clone();
    log.data = vec![0u8; DEPOSIT_DATA_LEN + 32].into();
    assert!(check_deposit_log(&log, bridge).is_err());
    let mut log = deposit;
    log.topics.push(B256::ZERO);
    assert!(check_deposit_log(&log, bridge).is_err());
}

#[test]
fn the_topic_is_the_events_signature() {
    assert_eq!(
        keccak256("Deposit(uint256,address,uint256,bytes32,uint256,address)"),
        DEPOSIT_TOPIC0
    );
}

#[test]
fn finalize_params_carry_the_abi_names() {
    let w = DepositWitness {
        chain_id: 421614,
        block_number: 1,
        block_hash: B256::ZERO,
        header: vec![0xf9, 0x01].into(),
        tx_index: 9,
        receipt_proof: vec![vec![0xc0].into()],
        log_index: 1,
    };
    assert_eq!(
        w.finalize_params(),
        json!({"chainId": "421614", "header": "f901", "txIndex": 9, "receiptProof": ["c0"], "logIndex": 1})
    );
    assert_eq!(w.payload_len(), 3);
}

/// The live Arbitrum Sepolia deposit transaction, read through a node that
/// answers `eth_getTransactionReceipt` with `receipt`; the block and its
/// receipts are the real ones. Its `Deposit` log predates the token field, so
/// a fetch that gets as far as the log ends in `NotDeposit`.
async fn fetch_with(receipt: serde_json::Value, log_index: usize) -> Result<DepositWitness, Error> {
    use alloy::{providers::ProviderBuilder, transports::mock::Asserter};
    let vector: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
    let asserter = Asserter::new();
    asserter.push_success(&receipt);
    asserter.push_success(&serde_json::from_str::<serde_json::Value>(BLOCK).unwrap());
    asserter.push_success(&serde_json::from_str::<serde_json::Value>(RECEIPTS).unwrap());
    let provider = ProviderBuilder::new().connect_mocked_client(asserter);
    fetch(
        &provider,
        421_614,
        "0xcee5d884bc1b7f4cc84a31fea74f3c8809d19bd5"
            .parse()
            .unwrap(),
        vector["blockHash"].as_str().unwrap().parse().unwrap(),
        vector["transactionHash"].as_str().unwrap().parse().unwrap(),
        log_index,
    )
    .await
}

/// The deposit transaction's receipt as `eth_getBlockReceipts` returns it.
fn block_receipt() -> serde_json::Value {
    let receipts: Vec<serde_json::Value> = serde_json::from_str(RECEIPTS).unwrap();
    receipts
        .into_iter()
        .find(|r| r["transactionIndex"] == "0x9")
        .unwrap()
}

#[tokio::test]
async fn the_verdict_is_read_from_the_receipt_that_rebuilt_the_block() {
    // A node behind a load balancer answers from a stale branch that the
    // transaction failed. The block's own receipt is a success, and only
    // that one decides: here, that its log is not the current Deposit.
    let mut stale = block_receipt();
    stale["status"] = "0x0".into();
    stale["logs"] = serde_json::json!([]);
    let got = fetch_with(stale, 1).await;
    assert!(
        matches!(
            got,
            Err(Error::NotDeposit {
                index: 1,
                ..
            })
        ),
        "{got:?}"
    );
    let got = fetch_with(block_receipt(), 0).await;
    assert!(
        matches!(
            got,
            Err(Error::NotDeposit {
                index: 0,
                ..
            })
        ),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_receipt_of_another_block_is_retried_never_a_verdict() {
    let mut elsewhere = block_receipt();
    elsewhere["blockHash"] = format!("0x{}", "11".repeat(32)).into();
    elsewhere["status"] = "0x0".into();
    let got = fetch_with(elsewhere, 1).await;
    assert!(matches!(got, Err(Error::BlockChanged(..))), "{got:?}");
}
