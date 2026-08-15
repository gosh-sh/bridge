//! TD-59 — `Deposit` event signature parity + `topic1` depositId filter regression.

use std::sync::Arc;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    source::{AckiNackiBridge::Deposit, DepositSource},
    types::DepositEvent,
};

/// Canonical `keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")`.
const CANONICAL_DEPOSIT_EVENT_SIG: [u8; 32] = [
    0x8d, 0x5d, 0x06, 0x06, 0x73, 0xb2, 0x7f, 0xac, 0x84, 0xd5, 0x6e, 0xe2, 0x62, 0xfe, 0x8d,
    0xcc, 0xad, 0x60, 0xd1, 0x98, 0xae, 0x11, 0x76, 0x60, 0x63, 0xf1, 0x12, 0xa9, 0xbe, 0x3d,
    0x37, 0xee,
];

fn eth_log_topic1_for_deposit_id(deposit_id: u64) -> B256 {
    B256::from(U256::from(deposit_id).to_be_bytes::<32>())
}

fn sample_event(deposit_id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64 + deposit_id),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + deposit_id),
        tx_hash: B256::repeat_byte(0xaa + deposit_id as u8),
        log_index: 0,
        block_number: 100 + deposit_id,
        block_hash: B256::repeat_byte(0xbb),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

/// Mirrors `EthLogSource::fetch` topic0/topic1 filter (no RPC).
struct SignatureFilteredSource {
    entries: Vec<(alloy::primitives::FixedBytes<32>, B256, DepositEvent)>,
}

impl SignatureFilteredSource {
    fn with_entries(entries: Vec<(alloy::primitives::FixedBytes<32>, B256, DepositEvent)>) -> Arc<Self> {
        Arc::new(Self { entries })
    }
}

#[async_trait]
impl DepositSource for SignatureFilteredSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        let topic1 = eth_log_topic1_for_deposit_id(deposit_id);
        let sig = Deposit::SIGNATURE_HASH;
        for (topic0, t1, event) in &self.entries {
            if *topic0 == sig && *t1 == topic1 && event.deposit_id == deposit_id {
                return Ok(Some(event.clone()));
            }
        }
        Ok(None)
    }
}

#[test]
fn td_59_relayer_signature_hash_matches_canonical_and_prover_hex() {
    let relayer_sig = Deposit::SIGNATURE_HASH;
    assert_eq!(B256::from(relayer_sig), B256::from(CANONICAL_DEPOSIT_EVENT_SIG));
}

#[test]
fn td_59_topic1_be_encoding_matches_eth_log_source_filter() {
    assert_eq!(
        eth_log_topic1_for_deposit_id(0),
        B256::ZERO,
        "depositId 0 → zero topic1"
    );
    assert_eq!(
        eth_log_topic1_for_deposit_id(1),
        B256::from(U256::from(1u64)),
        "depositId 1 → BE uint256 in topic1"
    );
    assert_eq!(
        eth_log_topic1_for_deposit_id(u64::MAX),
        B256::from(U256::from(u64::MAX)),
        "depositId u64::MAX"
    );
}

#[tokio::test]
async fn td_59_wrong_topic0_fetch_returns_none() {
    let event = sample_event(7);
    let wrong_sig = keccak256("DepositLegacy(uint256,address,uint256,int8,bytes32,uint256)");
    let right_topic1 = eth_log_topic1_for_deposit_id(7);

    let source = SignatureFilteredSource::with_entries(vec![(wrong_sig, right_topic1, event.clone())]);
    assert!(
        source.fetch(7).await.unwrap().is_none(),
        "wrong topic0 must not match deposit scan"
    );

    let source_ok = SignatureFilteredSource::with_entries(vec![
        (
            Deposit::SIGNATURE_HASH,
            right_topic1,
            event,
        ),
    ]);
    assert!(
        source_ok.fetch(7).await.unwrap().is_some(),
        "canonical topic0 + topic1 must match"
    );
}
