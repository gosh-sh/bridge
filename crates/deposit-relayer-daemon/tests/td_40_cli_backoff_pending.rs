//! TD-40 / DEP-BACKOFF / D-16 — CLI backoff=0 guard, AN `Pending` retry path,
//! `RelayerState::save` atomic write (cross-ref TD-18).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    BackoffConfig,
    error::RelayerError,
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    state::RelayerState,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
};
use tempfile::tempdir;

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_600u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn repo_deposit_prover_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deposit-prover")
}

fn deposit_relayer_bin() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_BIN_EXE_deposit-relayer")
            .expect("integration test requires deposit-relayer binary"),
    )
}

/// Simulates `confirm_timeout_secs` → `SubmitOutcome::Pending` once per id.
struct TimeoutPendingOnceSubmitter {
    inner: Arc<MockAnSubmitter>,
    pending_once: Mutex<HashMap<u64, bool>>,
}

impl TimeoutPendingOnceSubmitter {
    fn new(inner: Arc<MockAnSubmitter>) -> Self {
        Self {
            inner,
            pending_once: Mutex::new(HashMap::new()),
        }
    }

    fn arm_timeout_pending(&self, deposit_id: u64) {
        self.pending_once
            .lock()
            .expect("poisoned")
            .insert(deposit_id, true);
    }
}

#[async_trait]
impl AnSubmitter for TimeoutPendingOnceSubmitter {
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError> {
        self.inner.is_finalized(deposit_id).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        let pending = {
            let mut arms = self.pending_once.lock().expect("poisoned");
            if arms.get(&event.deposit_id) == Some(&true) {
                arms.insert(event.deposit_id, false);
                true
            } else {
                false
            }
        };
        if pending {
            return Ok(SubmitOutcome::Pending {
                reason: "confirmation timeout after confirm_timeout_secs".to_string(),
            });
        }
        self.inner.submit(event, bundle).await
    }
}

type R<A> = Relayer<InMemoryDepositSource, MockProofGenerator, A>;

fn make_relayer<A: AnSubmitter>(
    source: Arc<InMemoryDepositSource>,
    prover: Arc<MockProofGenerator>,
    submitter: Arc<A>,
    state_path: PathBuf,
    scan_cursor: Option<Arc<Mutex<u64>>>,
) -> R<A> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.scan_cursor = scan_cursor;
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

#[test]
fn td_40_a_backoff_initial_zero_validate_err() {
    let cfg = BackoffConfig {
        initial: Duration::ZERO,
        max: Duration::from_secs(120),
        multiplier: 2,
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("backoff initial delay must be > 0"),
        "TD-40: zero initial must not allow hot loop"
    );
}

#[test]
fn td_40_b_backoff_multiplier_zero_validate_err() {
    let cfg = BackoffConfig {
        initial: Duration::from_secs(5),
        max: Duration::from_secs(120),
        multiplier: 0,
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("backoff multiplier must be >= 1"),
        "TD-40: zero multiplier must not allow hot loop"
    );
}

#[tokio::test]
async fn td_40_c_an_pending_retry_no_deposit_cursor_advance() {
    const FROM_BLOCK: u64 = 90;
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let scan_cursor = Arc::new(Mutex::new(FROM_BLOCK));
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let inner = Arc::new(MockAnSubmitter::accepting());
    let submitter = Arc::new(TimeoutPendingOnceSubmitter::new(inner));
    submitter.arm_timeout_pending(0);

    let mut relayer = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
        state_path.clone(),
        Some(scan_cursor.clone()),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnPending { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert_eq!(relayer.state().attempts_since_progress, 1);
    assert_eq!(*scan_cursor.lock().expect("poisoned"), FROM_BLOCK);
    assert_eq!(
        relayer.state().scanned_through_block,
        Some(FROM_BLOCK),
        "AnPending persist must not advance scan cursor"
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}

#[test]
fn td_40_d_state_save_atomic_smoke_corrupt_tmp_ignored() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let tmp = dir.path().join("state.json.tmp");

    let mut state = RelayerState::default();
    state.record_progress(3);
    state.scanned_through_block = Some(200);
    state.save(&path).unwrap();

    assert!(path.exists());
    std::fs::write(&tmp, b"{\"last_processed_deposit_id\": 99").unwrap();

    let loaded = RelayerState::load(&path).unwrap().unwrap();
    assert_eq!(loaded.last_processed_deposit_id, Some(3));
    assert_eq!(loaded.scanned_through_block, Some(200));

    state.record_progress(5);
    state.save(&path).unwrap();
    let reloaded = RelayerState::load(&path).unwrap().unwrap();
    assert_eq!(reloaded.last_processed_deposit_id, Some(5));
}

#[test]
fn td_40_cli_backoff_initial_zero_startup_rejects() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let prover_dir = repo_deposit_prover_dir();
    assert!(prover_dir.is_dir(), "deposit-prover dir missing for CLI test");

    let output = std::process::Command::new(deposit_relayer_bin())
        .arg("--state")
        .arg(state_path.as_os_str())
        .args([
            "daemon",
            "--rpc-url",
            "http://127.0.0.1:9",
            "--bridge-address",
            "0x0000000000000000000000000000000000000001",
            "--deposit-prover-dir",
            prover_dir.as_os_str().to_str().expect("utf8 path"),
            "--dry-run",
            "--backoff-initial-secs",
            "0",
        ])
        .output()
        .expect("spawn deposit-relayer");

    assert!(
        !output.status.success(),
        "TD-40: daemon must reject backoff_initial_secs=0 at startup"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("backoff initial delay must be > 0"),
        "expected validate error in output: {combined}"
    );
    assert!(!state_path.exists(), "daemon must fail before state lock");
}

#[test]
fn td_40_cli_backoff_multiplier_zero_startup_rejects() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let prover_dir = repo_deposit_prover_dir();

    let output = std::process::Command::new(deposit_relayer_bin())
        .arg("--state")
        .arg(state_path.as_os_str())
        .args([
            "daemon",
            "--rpc-url",
            "http://127.0.0.1:9",
            "--bridge-address",
            "0x0000000000000000000000000000000000000001",
            "--deposit-prover-dir",
            prover_dir.as_os_str().to_str().expect("utf8 path"),
            "--dry-run",
            "--backoff-multiplier",
            "0",
        ])
        .output()
        .expect("spawn deposit-relayer");

    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("backoff multiplier must be >= 1"));
}
