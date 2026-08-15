//! TD-39 / QC-OFF-10 — incremental scan vs O(head) `eth_getLogs` chunk cost.
//!
//! Mirrors `EthLogSource::fetch` chunk loop (`GET_LOGS_CHUNK_BLOCKS = 10`) and
//! TD-06 policy: targeted fetch does **not** advance `scan_cursor`.

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
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

const FROM_BLOCK: u64 = 90;
const CHUNK_BLOCKS: u64 = 10;
const BLOCK_0: u64 = 100;
const BLOCK_1: u64 = 101;

fn effective_scan_from(from_block: u64, cursor_val: u64) -> u64 {
    if cursor_val >= from_block {
        cursor_val.saturating_add(1)
    } else {
        from_block
    }
}

/// Same chunk iteration count as `EthLogSource::fetch` (10-block windows).
fn count_get_logs_chunks(scan_from: u64, safe_head: u64) -> u32 {
    if safe_head < scan_from {
        return 0;
    }
    let mut n = 0u32;
    let mut chunk_start = scan_from;
    while chunk_start <= safe_head {
        let chunk_end = chunk_start
            .saturating_add(CHUNK_BLOCKS - 1)
            .min(safe_head);
        n += 1;
        chunk_start = chunk_end.saturating_add(1);
    }
    n
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

/// Mock `EthLogSource` scan semantics with per-fetch chunk accounting.
struct ChunkCountingSource {
    from_block: u64,
    scan_cursor: Arc<Mutex<u64>>,
    safe_head: Mutex<u64>,
    chunk_log: Mutex<Vec<u32>>,
    events: Mutex<HashMap<u64, DepositEvent>>,
}

impl ChunkCountingSource {
    fn new(from_block: u64, scan_cursor: Arc<Mutex<u64>>) -> Self {
        Self {
            from_block,
            scan_cursor,
            safe_head: Mutex::new(from_block),
            chunk_log: Mutex::new(Vec::new()),
            events: Mutex::new(HashMap::new()),
        }
    }

    fn insert(&self, event: DepositEvent) {
        self.events
            .lock()
            .expect("poisoned")
            .insert(event.deposit_id, event);
    }

    fn set_safe_head(&self, head: u64) {
        *self.safe_head.lock().expect("poisoned") = head;
    }

    fn chunk_log(&self) -> Vec<u32> {
        self.chunk_log.lock().expect("poisoned").clone()
    }

    fn scan_cursor_value(&self) -> u64 {
        *self.scan_cursor.lock().expect("poisoned")
    }
}

#[async_trait]
impl DepositSource for ChunkCountingSource {
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
        let cursor = *self.scan_cursor.lock().expect("poisoned");
        let scan_from = effective_scan_from(self.from_block, cursor);
        let safe_head = *self.safe_head.lock().expect("poisoned");
        let chunks = count_get_logs_chunks(scan_from, safe_head);
        self.chunk_log.lock().expect("poisoned").push(chunks);

        if event.block_number < scan_from {
            return Ok(None);
        }
        Ok(Some(event))
    }
}

fn shared_scan_cursor(initial: u64) -> Arc<Mutex<u64>> {
    Arc::new(Mutex::new(initial))
}

type R<S> = Relayer<S, MockProofGenerator, MockAnSubmitter>;

fn make_relayer<S: DepositSource>(
    source: Arc<S>,
    prover: Arc<MockProofGenerator>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
    scan_cursor: Arc<Mutex<u64>>,
) -> R<S> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.scan_cursor = Some(scan_cursor);
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

/// (a) Growing `safe_head` with fixed cursor → O(head) chunk calls per tick.
#[tokio::test]
async fn td_39_a_safe_head_growth_increases_chunk_calls_per_tick() {
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ChunkCountingSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_0));

    source.set_safe_head(100);
    source.fetch(0).await.unwrap();
    let c100 = source.chunk_log().last().copied().unwrap();

    source.set_safe_head(120);
    source.fetch(0).await.unwrap();
    let c120 = source.chunk_log().last().copied().unwrap();

    source.set_safe_head(140);
    source.fetch(0).await.unwrap();
    let c140 = source.chunk_log().last().copied().unwrap();

    assert_eq!(c100, count_get_logs_chunks(91, 100));
    assert_eq!(c120, count_get_logs_chunks(91, 120));
    assert_eq!(c140, count_get_logs_chunks(91, 140));
    assert!(c100 < c120 && c120 < c140, "TD-39: O(head) rescan per targeted fetch");
}

/// (b) Advancing scan cursor to tail shrinks next fetch to O(new blocks).
#[tokio::test]
async fn td_39_b_advanced_cursor_reduces_chunk_calls() {
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ChunkCountingSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, 140));
    source.set_safe_head(140);

    source.fetch(0).await.unwrap();
    let full = source.chunk_log().last().copied().unwrap();
    assert_eq!(full, count_get_logs_chunks(91, 140));

    *scan_cursor.lock().expect("poisoned") = 139;
    source.fetch(0).await.unwrap();
    let tail = source.chunk_log().last().copied().unwrap();
    assert_eq!(tail, count_get_logs_chunks(140, 140));
    assert_eq!(tail, 1);
    assert!(tail < full, "incremental tail scan is cheaper than full rescan");
}

/// (c) TD-06 regression: targeted fetch does not advance cursor; sequential ids visible.
#[tokio::test]
async fn td_39_c_targeted_fetch_does_not_advance_cursor_sequential_ok() {
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ChunkCountingSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_0));
    source.insert(deposit_at_block(1, BLOCK_1));
    source.set_safe_head(120);

    assert!(source.fetch(0).await.unwrap().is_some());
    assert_eq!(source.scan_cursor_value(), FROM_BLOCK);

    assert!(source.fetch(1).await.unwrap().is_some());
    assert_eq!(source.scan_cursor_value(), FROM_BLOCK);
}

/// (d) TD-07 cross-ref: proof failure does not move deposit cursor; scan cost is separate.
#[tokio::test]
async fn td_39_d_proof_fail_preserves_deposit_cursor_scan_cost_separate() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = shared_scan_cursor(FROM_BLOCK);
    let source = Arc::new(ChunkCountingSource::new(FROM_BLOCK, scan_cursor.clone()));
    source.insert(deposit_at_block(0, BLOCK_0));
    source.set_safe_head(120);

    let prover = Arc::new(MockProofGenerator::failing_on(0));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source.clone(),
        prover,
        submitter.clone(),
        state_path,
        scan_cursor.clone(),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::ProofFailed { deposit_id: 0, .. }
    ));

    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(source.scan_cursor_value(), FROM_BLOCK);
    assert_eq!(
        relayer.state().scanned_through_block,
        Some(FROM_BLOCK),
        "persisted scan cursor unchanged on proof failure"
    );
    assert!(
        source.chunk_log().last().copied().unwrap_or(0) > 0,
        "fetch still paid O(head) chunk RPC cost"
    );
    assert_eq!(submitter.finalized_count(), 0);
    assert!(
        source.fetch(0).await.unwrap().is_some(),
        "deposit still discoverable after proof failure"
    );
}
