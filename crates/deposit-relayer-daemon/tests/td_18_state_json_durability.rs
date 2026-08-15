//! TD-18 — `state.json` durability: atomic save, corrupt load, partial tmp,
//! deployment mismatch + `--force-state` cursor reset (TD-07 persistence pattern).

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    state::{DeploymentIdentity, RelayerState},
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

const CHAIN: u64 = 11_155_111;
const BRIDGE: Address = Address::repeat_byte(0x99);

fn deployment(dapp_id: &str) -> DeploymentIdentity {
    DeploymentIdentity::new(CHAIN, BRIDGE, dapp_id)
}

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_500u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: BRIDGE,
        source_chain_id: CHAIN,
    }
}

fn make_relayer(
    source: Arc<InMemoryDepositSource>,
    state_path: PathBuf,
    deployment: DeploymentIdentity,
    force_state: bool,
    start_deposit_id: u64,
) -> Relayer<InMemoryDepositSource, MockProofGenerator, MockAnSubmitter> {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.deployment = Some(deployment);
    cfg.force_state = force_state;
    cfg.start_deposit_id = start_deposit_id;
    Relayer::new(
        cfg,
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .unwrap()
}

#[test]
fn td_18_atomic_save_survives_read_back() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");

    let mut state = RelayerState::default();
    state.ensure_deployment(&deployment("0xAAAA"), false).unwrap();
    state.record_progress(7);
    state.scanned_through_block = Some(120);
    state.save(&path).unwrap();

    let loaded = RelayerState::load(&path).unwrap().unwrap();
    assert_eq!(loaded.last_processed_deposit_id, Some(7));
    assert_eq!(loaded.scanned_through_block, Some(120));
    assert_eq!(loaded.deployment, Some(deployment("0xAAAA")));
}

#[test]
fn td_18_corrupt_state_json_safe_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, b"{bad-json").unwrap();
    assert!(RelayerState::load(&path).is_err());
}

#[test]
fn td_18_partial_tmp_without_rename_not_loaded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let tmp = dir.path().join("state.json.tmp");

    std::fs::write(&tmp, b"{\"last_processed_deposit_id\": 99").unwrap();
    assert_eq!(RelayerState::load(&path).unwrap(), None);

    let mut state = RelayerState::default();
    state.record_progress(4);
    state.save(&path).unwrap();
    std::fs::write(&tmp, b"{\"last_processed_deposit_id\": 99").unwrap();

    let loaded = RelayerState::load(&path).unwrap().unwrap();
    assert_eq!(loaded.last_processed_deposit_id, Some(4));
}

#[test]
fn td_18_deployment_mismatch_without_force_rejects() {
    let mut state = RelayerState {
        deployment: Some(deployment("0xAAAA")),
        last_processed_deposit_id: Some(5),
        ..RelayerState::default()
    };
    let err = state
        .ensure_deployment(&deployment("0xBBBB"), false)
        .unwrap_err();
    assert!(err.to_string().contains("deployment mismatch"));
    assert_eq!(state.last_processed_deposit_id, Some(5));
}

#[test]
fn td_18_force_state_resets_cursor_on_dapp_mismatch() {
    let mut state = RelayerState {
        deployment: Some(deployment("0xAAAA")),
        last_processed_deposit_id: Some(5),
        last_attempt_deposit_id: Some(5),
        scanned_through_block: Some(200),
        parked_deposit_ids: vec![3],
        attempts_since_progress: 4,
        ..RelayerState::default()
    };
    state.ensure_deployment(&deployment("0xBBBB"), true).unwrap();
    assert_eq!(state.deployment, Some(deployment("0xBBBB")));
    assert_eq!(state.last_processed_deposit_id, None);
    assert_eq!(state.next_target(0), 0);
    assert_eq!(state.scanned_through_block, None);
    assert!(state.parked_deposit_ids.is_empty());
}

#[tokio::test]
async fn td_18_relayer_startup_rejects_stale_namespace_without_force() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");

    let mut persisted = RelayerState {
        deployment: Some(deployment("0xAAAA")),
        last_processed_deposit_id: Some(5),
        ..RelayerState::default()
    };
    persisted.save(&path).unwrap();

    let source = Arc::new(InMemoryDepositSource::new());
    let mut cfg = RelayerConfig::new(path);
    cfg.deployment = Some(deployment("0xBBBB"));
    cfg.force_state = false;

    assert!(Relayer::new(
        cfg,
        source,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
    )
    .is_err());
}

#[tokio::test]
async fn td_18_relayer_force_state_does_not_inherit_wrong_namespace_cursor() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");

    let mut persisted = RelayerState {
        deployment: Some(deployment("0xAAAA")),
        last_processed_deposit_id: Some(5),
        ..RelayerState::default()
    };
    persisted.save(&path).unwrap();

    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let mut relayer = make_relayer(
        source,
        path,
        deployment("0xBBBB"),
        true,
        0,
    );

    assert_eq!(relayer.state().next_target(0), 0);
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}

#[tokio::test]
async fn td_18_crash_sim_no_double_mint_after_progress_persisted() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0));

    let mut relayer = make_relayer(
        source.clone(),
        path.clone(),
        deployment("0xAAAA"),
        false,
        0,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));

    let reloaded = RelayerState::load(&path).unwrap().unwrap();
    assert_eq!(reloaded.last_processed_deposit_id, Some(0));

    source.insert(deposit(1));
    let mut restarted = make_relayer(source, path, deployment("0xAAAA"), false, 0);
    assert_eq!(restarted.state().next_target(0), 1);
    assert!(matches!(
        restarted.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 1, .. }
    ));
}
