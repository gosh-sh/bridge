//! TD-59 — deposit event signature regression (prover + fixture receipt topic0).

use deposit_prover::{
    circuit_v2::get_deposit_event_signature as circuit_signature,
    ethereum_fetcher::get_deposit_event_signature as fetcher_signature,
    types::DepositProofInput,
};
use ethers::types::{H256, Log};
use ethers_core::utils::rlp::Rlp;

const CANONICAL_DEPOSIT_EVENT_SIG: [u8; 32] = [
    0x8d, 0x5d, 0x06, 0x06, 0x73, 0xb2, 0x7f, 0xac, 0x84, 0xd5, 0x6e, 0xe2, 0x62, 0xfe, 0x8d,
    0xcc, 0xad, 0x60, 0xd1, 0x98, 0xae, 0x11, 0x76, 0x60, 0x63, 0xf1, 0x12, 0xa9, 0xbe, 0x3d,
    0x37, 0xee,
];

fn proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn decode_receipt_logs(receipt_rlp: &[u8]) -> Vec<Log> {
    let payload = if receipt_rlp.first() == Some(&0x02) {
        &receipt_rlp[1..]
    } else {
        receipt_rlp
    };
    let rlp = Rlp::new(payload);
    let logs_rlp = rlp.at(3).expect("receipt logs field");
    let mut logs = Vec::new();
    for i in 0..logs_rlp.item_count().expect("logs list") {
        let log_rlp = logs_rlp.at(i).expect("log entry");
        let addr_bytes: Vec<u8> = log_rlp
            .at(0)
            .expect("addr")
            .as_val()
            .expect("address decode");
        let address = ethers::types::Address::from_slice(&addr_bytes);
        let topics_rlp = log_rlp.at(1).expect("topics");
        let mut topics = Vec::new();
        for j in 0..topics_rlp.item_count().expect("topic count") {
            let topic_bytes: Vec<u8> = topics_rlp
                .at(j)
                .expect("topic")
                .as_val()
                .expect("topic decode");
            topics.push(H256::from_slice(&topic_bytes));
        }
        let data: Vec<u8> = log_rlp
            .at(2)
            .expect("data")
            .as_val()
            .expect("data decode");
        logs.push(Log {
            address,
            topics,
            data: ethers::types::Bytes::from(data),
            ..Default::default()
        });
    }
    logs
}

#[test]
fn td_59_fetcher_and_circuit_signatures_stable_and_equal() {
    let fetcher = fetcher_signature();
    let circuit = circuit_signature();
    assert_eq!(fetcher, circuit);
    assert_eq!(fetcher, CANONICAL_DEPOSIT_EVENT_SIG);
    assert_eq!(fetcher_signature(), fetcher_signature());
}

#[test]
fn td_59_proof_00_receipt_deposit_log_topic0_matches_canonical() {
    let input = proof_00();
    let canonical = H256::from(fetcher_signature());
    let logs = decode_receipt_logs(&input.receipt_proof.receipt_rlp);
    let log_index = input.event_data.log_index as usize;
    let deposit_log = &logs[log_index];

    assert_eq!(deposit_log.topics[0], canonical, "topic0 signature");
    assert_eq!(
        deposit_log.topics[1],
        H256::from_low_u64_be(input.event_data.deposit_id),
        "topic1 depositId"
    );
}
