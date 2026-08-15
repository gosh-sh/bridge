//! TD-62 — off-chain sequential target vs on-chain dense `depositId` counter.
//!
//! L1 ids are always dense `0..depositCounter-1`. Relayer `next_target` walks
//! sequentially; missing lower ids → HOL stall (`NotYetAvailable`), never silent skip.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::{InMemoryDepositSource},
    state::RelayerState,
    submitter::MockAnSubmitter,
    types::DepositEvent,
};
use tempfile::tempdir;

fn deposit(id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64 + id),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number: 100 + id,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn relayer_cfg(state_path: PathBuf, start_deposit_id: u64) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg.start_deposit_id = start_deposit_id;
    cfg
}

fn write_state(path: &PathBuf, state: &RelayerState) {
    std::fs::write(path, serde_json::to_string(state).unwrap()).unwrap();
}

/// Optional operator hint — mirrors `EthLogSource::deposit_counter()` (not used in tick).
struct OnChainCounterHint {
    counter: u64,
}

#[async_trait]
trait DepositCounterHint: Send + Sync {
    fn on_chain_deposit_counter(&self) -> u64;
}

impl DepositCounterHint for OnChainCounterHint {
    fn on_chain_deposit_counter(&self) -> u64 {
        self.counter
    }
}

/// (a) Only id 2 staged; start at 0 → HOL on 0/1, never finalize 2 ahead of target.
#[tokio::test]
async fn td_62_a_staged_id_two_hol_on_zero_one_no_finalize_two() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(2));

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), 0),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    for _ in 0..2 {
        assert!(
            matches!(
                relayer.tick().await.unwrap(),
                TickOutcome::NotYetAvailable { deposit_id: 0 }
            ),
            "TD-62 (a): HOL on target 0 (ids 1+ also blocked)"
        );
    }
    assert_eq!(relayer.state().next_target(0), 0);
    assert!(submitter.finalized_log().is_empty(), "must not finalize id 2 while target=0");
}

/// (b) `last_processed=1`, target=2, empty source → stall without advance.
#[tokio::test]
async fn td_62_b_last_processed_one_target_two_stalls() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    write_state(
        &state_path,
        &RelayerState {
            last_processed_deposit_id: Some(1),
            last_attempt_deposit_id: None,
            attempts_since_progress: 0,
            parked_deposit_ids: vec![],
            scanned_through_block: None,
            deployment: None,
        },
    );

    let source = Arc::new(InMemoryDepositSource::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path, 0),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter,
    )
    .unwrap();

    assert_eq!(relayer.state().next_target(0), 2);
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 2 }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(1));
}

/// (c) Operator QC: on-chain counter < target → permanent HOL until chain catches up.
#[tokio::test]
async fn td_62_c_target_ahead_of_on_chain_counter_stalls_qc() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    write_state(
        &state_path,
        &RelayerState {
            last_processed_deposit_id: Some(4),
            last_attempt_deposit_id: None,
            attempts_since_progress: 0,
            parked_deposit_ids: vec![],
            scanned_through_block: None,
            deployment: None,
        },
    );

    let hint = OnChainCounterHint { counter: 3 };
    assert!(
        5 > hint.on_chain_deposit_counter(),
        "TD-62 QC: target 5 ahead of on-chain counter 3"
    );

    let source = Arc::new(InMemoryDepositSource::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(state_path, 0),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert_eq!(relayer.state().next_target(0), 5);
    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 5 }
    ));
    assert!(submitter.finalized_log().is_empty());
}

/// (d) Sequential deps 0,1,2 → three ticks finalize in order.
#[tokio::test]
async fn td_62_d_sequential_deposits_finalize_zero_one_two() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    for id in 0..3 {
        source.insert(deposit(id));
    }

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json"), 0),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    for expected_id in 0..3 {
        assert!(matches!(
            relayer.tick().await.unwrap(),
            TickOutcome::Finalized { deposit_id: expected_id, .. }
        ));
    }
    assert_eq!(submitter.finalized_log(), vec![0, 1, 2]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(2));
}
