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
    let types: std::collections::BTreeSet<u64> =
        receipts.iter().map(|r| r.ty.map_or(0, |t| t.to())).collect();
    assert_eq!(types, [0, 2, 0x6a].into());

    let values: Vec<Vec<u8>> = receipts.iter().map(RpcReceipt::trie_value).collect();
    let vector: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
    let fd = &vector["finalizeDeposit"];
    let index = fd["txIndex"].as_u64().unwrap() as usize;
    let (root, proof) = receipt_proof(&values, index);
    assert_eq!(root, header.inner.receipts_root);
    let expected: Vec<Vec<u8>> =
        fd["receiptProof"].as_array().unwrap().iter().map(|p| unhex(p.as_str().unwrap())).collect();
    assert_eq!(proof.iter().map(|p| p.to_vec()).collect::<Vec<_>>(), expected);

    let rlp = alloy::rlp::encode(&header.inner);
    assert_eq!(keccak256(&rlp), header.hash);
    assert_eq!(rlp, unhex(fd["header"].as_str().unwrap()));
}

#[test]
fn only_the_bridges_six_field_deposit_passes_the_shape_check() {
    let bridge = Address::repeat_byte(0x42);
    let deposit = RpcLog {
        address: bridge,
        topics: vec![DEPOSIT_TOPIC0, B256::with_last_byte(7), B256::with_last_byte(0xab)],
        data: vec![0u8; DEPOSIT_DATA_LEN].into(),
    };
    assert_eq!(check_deposit_log(&deposit, bridge), Ok(B256::with_last_byte(7)));
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
    assert_eq!(keccak256("Deposit(uint256,address,uint256,bytes32,uint256,address)"), DEPOSIT_TOPIC0);
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
