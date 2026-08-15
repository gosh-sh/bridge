//! TD-07 — scan cursor + proof failure: production `EthLogSource` cursor must not
//! skip the target block on prove/submit failure; restart must rediscover the deposit.
//!
//! Catalog `TD-07`: mirrors `EthLogSource::effective_scan_from` + persisted
//! `scanned_through_block` / `scan_cursor` (see `td_06_scan_cursor.rs`).

use std::{
    collections::HashMap,
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
    source::DepositSource,
    state::RelayerState,
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

const FROM_BLOCK: u64 = 90;
const BLOCK_N: u64 = 101;
const SAFE_HEAD: u64 = 120;

fn effective_scan_from(from_block: u64, cursor_val: u64) -> u64 {
    if cursor_val >= from_block {
        cursor_val.saturating_add(1)
    } else {
        from_block
    }
}

/// Production-shaped source: deposit visible only when `block_number >= scan_from`.
struct ScanCursorDepositSource {
    from_block: u64,
    scan_cursor: Arc<Mutex<u64>>,
    events: Mutex<HashMap<u64, DepositEvent>>,
}

impl ScanCursorDepositSource {
    fn new(from_block: u64, scan_cursor: Arc<Mutex<u64>>) -> Self {
        Self {
            from_block,
            scan_cursor,
            events: Mutex::new(HashMap::new()),
        }
    }

    fn insert(&self, event: DepositEvent) {
        self.events
            .lock()
            .expect("poisoned")
            .insert(event.deposit_id, event);
    }

    async fn scan_cursor_value(&self) -> u64 {
        *self.scan_cursor.lock().expect("poisoned")
    }
}

#[async_trait]
impl DepositSource for ScanCursorDepositSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        let event = self.events.lock().expect("poisoned").get(&deposit_id).cloned();
        if event.is_none() {
            return Ok(None);
        }
        let event = event.unwrap();
        let cursor = *self.scan_cursor.lock().expect("poisoned");
        let scan_from = effective_scan_from(self.from_block, cursor);
        if event.block_number < scan_from {
            return Ok(None);
        }
        Ok(Some(event))
    }
}

fn deposit_at_block(deposit_id: u64, block_number: u64) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_300u64 + deposit_id),
        tx_hash: B256::repeat_byte(0xaa + deposit_id as u8),
        log_index: 0,
        block_number,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

type R<S, P> = Relayer<S, P, MockAnSubmitter>;

fn make_relayer<S, P>(
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
    scan_cursor: Arc<Mutex<u64>>,
    start_deposit_id: u64,
) -> R<S, P>
where
    S: DepositSource,
    P: deposit_relayer_daemon::prover::ProofGenerator,
{
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.start_deposit_id = start_deposit_id;
    cfg.scan_cursor = Some(scan_cursor);
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

fn shared_scan_cursor(initial: u64) -> Arc<Mutex<u64>> {
    Arc::new(Mutex::new(initial))
}

#[tokio::test]
async fn td_07_proof_failure_does_not_advance_past_target_block() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ScanCursorDepositSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_N));

    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source.clone(),
        prover,
        submitter.clone(),
        state_path.clone(),
        scan_cursor.clone(),
        0,
    );

    match relayer.tick().await.unwrap() {
        TickOutcome::ProofFailed { deposit_id: 0, .. } => {},
        other => panic!("TD-07: expected ProofFailed, got {other:?}"),
    }

    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(relayer.state().next_target(0), 0);
    assert_eq!(source.scan_cursor_value().await, FROM_BLOCK);
    assert_eq!(
        relayer.state().scanned_through_block,
        Some(FROM_BLOCK),
        "persisted scan cursor must not jump on proof failure"
    );
    assert_eq!(submitter.finalized_count(), 0);

    // TD-06 regression shape: advancing cursor to safe_head would hide block N.
    let buggy_scan_from = effective_scan_from(FROM_BLOCK, SAFE_HEAD);
    assert!(
        BLOCK_N < buggy_scan_from,
        "pre-fix cursor=safe_head would skip block {BLOCK_N}"
    );
    assert!(
        source.fetch(0).await.unwrap().is_some(),
        "deposit at block {BLOCK_N} still discoverable after proof failure"
    );
}

#[tokio::test]
async fn td_07_restart_from_state_rediscovers_after_proof_failure() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ScanCursorDepositSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_N));

    let failing = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());

    let mut relayer = make_relayer(
        source.clone(),
        failing,
        submitter.clone(),
        state_path.clone(),
        scan_cursor.clone(),
        0,
    );
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));

    let loaded = RelayerState::load(&state_path)
        .unwrap()
        .expect("state.json must persist after proof failure");
    assert_eq!(loaded.last_processed_deposit_id, None);
    assert_eq!(loaded.scanned_through_block, Some(FROM_BLOCK));

    let restart_cursor = shared_scan_cursor(loaded.scanned_through_block.unwrap_or(FROM_BLOCK));
    let mut restarted = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
        state_path,
        restart_cursor,
        0,
    );

    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(restarted.state().last_processed_deposit_id, Some(0));
    assert_eq!(submitter.finalized_log(), vec![0]);
}

#[tokio::test]
async fn td_07_success_advances_deposit_cursor_sequential_finalize() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ScanCursorDepositSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, 100));
    source.insert(deposit_at_block(1, 101));

    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source.clone(),
        prover,
        submitter.clone(),
        state_path,
        scan_cursor.clone(),
        0,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
    assert_eq!(relayer.state().next_target(0), 1);

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(1));
    assert_eq!(submitter.finalized_log(), vec![0, 1]);
    assert_eq!(
        source.scan_cursor_value().await,
        FROM_BLOCK,
        "block scan cursor unchanged (TD-39 tail advance); deposit cursor advanced"
    );
}

#[tokio::test]
async fn td_07_an_reject_does_not_advance_deposit_cursor() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ScanCursorDepositSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_N));

    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::with_verifier(Arc::new(|_| false)));
    let mut relayer = make_relayer(
        source.clone(),
        prover,
        submitter,
        state_path,
        scan_cursor.clone(),
        0,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnRejected { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert!(
        source.fetch(0).await.unwrap().is_some(),
        "submit reject must not skip block {BLOCK_N}"
    );
}
