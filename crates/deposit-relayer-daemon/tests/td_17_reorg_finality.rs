//! TD-17 — reorg / finality: confirmation depth (`safe_head`) vs stale `blockHash`.
//!
//! Catalog `TD-17` / DEP-REORG / DEP-T15: production `EthLogSource` and
//! `fetch_deposit_from_receipt` gate on [`is_deposit_block_finalized`] before
//! surfacing deposits; AN anchor gate rejects proof-bound hashes outside the
//! accepted set (TD-03 cross).

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::{DepositSource, is_deposit_block_finalized},
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

const CONFIRMATIONS: u64 = 12;
const DEPOSIT_BLOCK: u64 = 98;
const HEAD_AT_SAFE: u64 = 110; // safe_head = 98
const HEAD_BELOW_SAFE: u64 = 109; // safe_head = 97
const SEPOLIA: u64 = 11_155_111;

const STALE_BLOCK_HASH: B256 = B256::repeat_byte(0xfa);
const CANONICAL_BLOCK_HASH: B256 = B256::repeat_byte(0xcd);

/// Production-shaped source: deposit visible only when buried under `confirmations`.
struct FinalityAwareDepositSource {
    confirmations: u64,
    chain_head: Arc<Mutex<u64>>,
    events: Mutex<HashMap<u64, DepositEvent>>,
}

impl FinalityAwareDepositSource {
    fn new(confirmations: u64, initial_head: u64) -> Self {
        Self {
            confirmations,
            chain_head: Arc::new(Mutex::new(initial_head)),
            events: Mutex::new(HashMap::new()),
        }
    }

    fn set_head(&self, head: u64) {
        *self.chain_head.lock().expect("poisoned") = head;
    }

    fn insert(&self, event: DepositEvent) {
        self.events
            .lock()
            .expect("poisoned")
            .insert(event.deposit_id, event);
    }

    fn update_block_hash(&self, deposit_id: u64, block_hash: B256) {
        let mut map = self.events.lock().expect("poisoned");
        if let Some(ev) = map.get_mut(&deposit_id) {
            ev.block_hash = block_hash;
        }
    }

    fn safe_head(&self) -> u64 {
        let head = *self.chain_head.lock().expect("poisoned");
        head.saturating_sub(self.confirmations)
    }
}

#[async_trait]
impl DepositSource for FinalityAwareDepositSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        let event = self
            .events
            .lock()
            .expect("poisoned")
            .get(&deposit_id)
            .cloned();
        if event.is_none() {
            return Ok(None);
        }
        let event = event.unwrap();
        let head = *self.chain_head.lock().expect("poisoned");
        if !is_deposit_block_finalized(event.block_number, head, self.confirmations) {
            return Ok(None);
        }
        Ok(Some(event))
    }
}

fn deposit_at_block(deposit_id: u64, block_number: u64, block_hash: B256) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_400u64 + deposit_id),
        tx_hash: B256::repeat_byte(0xaa + deposit_id as u8),
        log_index: 0,
        block_number,
        block_hash,
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: SEPOLIA,
    }
}

type R<S> = Relayer<S, MockProofGenerator, MockAnSubmitter>;

fn make_relayer<S: DepositSource>(
    source: Arc<S>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
    start_deposit_id: u64,
) -> R<S> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.start_deposit_id = start_deposit_id;
    Relayer::new(
        cfg,
        source,
        Arc::new(MockProofGenerator::new()),
        submitter,
    )
    .unwrap()
}

#[test]
fn td_17_deposit_above_safe_head_not_finalized() {
    // head=109, confirmations=12 → safe_head=97; block 98 is above safe window.
    assert!(!is_deposit_block_finalized(DEPOSIT_BLOCK, HEAD_BELOW_SAFE, CONFIRMATIONS));
    let safe = HEAD_BELOW_SAFE.saturating_sub(CONFIRMATIONS);
    assert!(
        DEPOSIT_BLOCK > safe,
        "TD-17: deposit block {DEPOSIT_BLOCK} must sit above safe_head {safe}"
    );
}

#[test]
fn td_17_at_safe_head_with_confirmations_12_ok() {
    // head=110, confirmations=12 → safe_head=98; block 98 is exactly at safe head.
    assert!(is_deposit_block_finalized(DEPOSIT_BLOCK, HEAD_AT_SAFE, CONFIRMATIONS));
    assert_eq!(HEAD_AT_SAFE.saturating_sub(CONFIRMATIONS), DEPOSIT_BLOCK);
}

#[test]
fn td_17_confirmations_zero_head_inclusive_documented() {
    // QC ops policy: confirmations=0 means safe_head=head (tip-inclusive).
    // CLI default is 12; zero is not forbidden but reorg-sensitive.
    assert!(is_deposit_block_finalized(100, 100, 0));
    assert!(!is_deposit_block_finalized(101, 100, 0));
}

#[tokio::test]
async fn td_17_mock_source_hides_deposit_until_safe_head() {
    let source = FinalityAwareDepositSource::new(CONFIRMATIONS, HEAD_BELOW_SAFE);
    source.insert(deposit_at_block(0, DEPOSIT_BLOCK, CANONICAL_BLOCK_HASH));

    assert!(
        source.fetch(0).await.unwrap().is_none(),
        "TD-17: block {DEPOSIT_BLOCK} above safe_head must not surface"
    );

    source.set_head(HEAD_AT_SAFE);
    assert!(
        source.fetch(0).await.unwrap().is_some(),
        "TD-17: at safe_head deposit must surface"
    );
}

#[tokio::test]
async fn td_17_simulated_reorg_head_drop_until_refinalize() {
    let source = FinalityAwareDepositSource::new(CONFIRMATIONS, HEAD_AT_SAFE);
    source.insert(deposit_at_block(0, DEPOSIT_BLOCK, CANONICAL_BLOCK_HASH));

    assert!(source.fetch(0).await.unwrap().is_some());

    // Simulated shallow reorg: head retreats so deposit block exits safe window.
    source.set_head(105); // safe_head = 93
    assert_eq!(source.safe_head(), 93);
    assert!(
        source.fetch(0).await.unwrap().is_none(),
        "TD-17: after head drop deposit must hide until re-finalized"
    );

    source.set_head(HEAD_AT_SAFE);
    assert!(
        source.fetch(0).await.unwrap().is_some(),
        "TD-17: after head recovery deposit must resurface"
    );
}

#[tokio::test]
async fn td_17_stale_block_hash_anchor_rejects_cross_td03() {
    let dir = tempdir().unwrap();
    let source = Arc::new(FinalityAwareDepositSource::new(CONFIRMATIONS, HEAD_AT_SAFE));
    source.insert(deposit_at_block(0, DEPOSIT_BLOCK, STALE_BLOCK_HASH));

    let submitter = Arc::new(MockAnSubmitter::with_anchor_gate(SEPOLIA, {
        let mut set = HashSet::new();
        set.insert(CANONICAL_BLOCK_HASH);
        set
    }));
    let mut relayer = make_relayer(
        source,
        submitter.clone(),
        dir.path().join("state.json"),
        0,
    );

    match relayer.tick().await.unwrap() {
        TickOutcome::AnRejected { deposit_id: 0, reason } => {
            assert!(reason.contains("ERR_UNKNOWN_BLOCK"));
            assert!(reason.contains("224"));
        },
        other => panic!("TD-17: stale blockHash must not finalize: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 0);
}

#[tokio::test]
async fn td_17_relayer_waits_until_head_catches_up() {
    let dir = tempdir().unwrap();
    let source = Arc::new(FinalityAwareDepositSource::new(CONFIRMATIONS, HEAD_BELOW_SAFE));
    source.insert(deposit_at_block(0, DEPOSIT_BLOCK, CANONICAL_BLOCK_HASH));

    let submitter = Arc::new(MockAnSubmitter::with_anchor_gate(SEPOLIA, {
        let mut set = HashSet::new();
        set.insert(CANONICAL_BLOCK_HASH);
        set
    }));
    let mut relayer = make_relayer(
        source.clone(),
        submitter.clone(),
        dir.path().join("state.json"),
        0,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 0 }
    ));
    assert_eq!(submitter.finalized_count(), 0);

    source.set_head(HEAD_AT_SAFE);
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);
}

#[tokio::test]
async fn td_17_hash_replace_after_reorg_finalize_on_canonical_hash() {
    let dir = tempdir().unwrap();
    let source = Arc::new(FinalityAwareDepositSource::new(CONFIRMATIONS, HEAD_AT_SAFE));
    source.insert(deposit_at_block(0, DEPOSIT_BLOCK, STALE_BLOCK_HASH));

    let submitter = Arc::new(MockAnSubmitter::with_anchor_gate(SEPOLIA, {
        let mut set = HashSet::new();
        set.insert(CANONICAL_BLOCK_HASH);
        set
    }));
    let mut relayer = make_relayer(
        source.clone(),
        submitter.clone(),
        dir.path().join("state.json"),
        0,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnRejected { deposit_id: 0, .. }
    ));

    // Post-reorg canonical hash at same block number (re-scan path).
    source.update_block_hash(0, CANONICAL_BLOCK_HASH);
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 1);
}
