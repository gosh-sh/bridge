//! F10-B — relayer fault injection (not blocked on QC-PROV-02).

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig},
    source::{DepositSource, InMemoryDepositSource},
    state::RelayerState,
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(id * 1000 + 1),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11155111,
    }
}

type R<S> = Relayer<S, MockProofGenerator, MockAnSubmitter>;

fn make_relayer<S: DepositSource + 'static>(
    source: Arc<S>,
    state_path: PathBuf,
) -> R<S> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    Relayer::new(
        cfg,
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap()
}

/// Returns a deposit whose `deposit_id` field does not match the requested key.
struct MismatchedIdSource {
    inner: InMemoryDepositSource,
}

#[async_trait]
impl DepositSource for MismatchedIdSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        let mut ev = self
            .inner
            .fetch(deposit_id)
            .await?
            .ok_or(RelayerError::DepositNotYetAvailable(deposit_id))?;
        ev.deposit_id = deposit_id.wrapping_add(1);
        Ok(Some(ev))
    }
}

#[tokio::test]
async fn corrupted_state_prevents_relayer_startup() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, b"{bad-json").unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    let mut cfg = RelayerConfig::new(path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    assert!(Relayer::new(
        cfg,
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .is_err());
}

#[tokio::test]
async fn source_deposit_id_mismatch_is_terminal() {
    let dir = tempdir().unwrap();
    let inner = InMemoryDepositSource::new();
    inner.insert(deposit(0));
    let source = Arc::new(MismatchedIdSource { inner });
    let mut relayer = make_relayer(source, dir.path().join("state.json"));

    let err = relayer.tick().await.unwrap_err();
    assert!(matches!(err, RelayerError::DepositIdMismatch { .. }));
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

#[test]
fn relayer_state_load_rejects_corrupted_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, b"[]").unwrap();
    assert!(RelayerState::load(&path).is_err());
}
