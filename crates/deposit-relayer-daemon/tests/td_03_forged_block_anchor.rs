//! TD-03 — forged block hash: circuit ok, AN anchor gate fail-closed.
//!
//! Catalog `TD-03`: proof-bound `blockHash` not in `_acceptedBlockHash` →
//! `ERR_UNKNOWN_BLOCK` (224); anchored control → finalize path.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome},
    types::{DepositEvent, DepositProofBundle},
};
use tempfile::tempdir;

const SEPOLIA: u64 = 11_155_111;
const EXIT_UNKNOWN_BLOCK: i32 = 224;

/// Proof-bound block hash for a forged / non-canonical L1 block (not in anchor set).
const FORGED_BLOCK_HASH: B256 = B256::repeat_byte(0xfa);

fn forged_deposit_event(deposit_id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_100u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 99_999,
        block_hash: FORGED_BLOCK_HASH,
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: SEPOLIA,
    }
}

fn forged_bundle(event: &DepositEvent) -> DepositProofBundle {
    let parsed = MockProofGenerator::derive_public_inputs(event, U256::ZERO);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde, 0xad].into(),
        parsed,
    }
}

fn anchor_gate_without_forged_hash() -> MockAnSubmitter {
    let mut accepted = HashSet::new();
    accepted.insert(B256::repeat_byte(0xcd));
    MockAnSubmitter::with_anchor_gate(SEPOLIA, accepted)
}

type R = Relayer<InMemoryDepositSource, MockProofGenerator, MockAnSubmitter>;

fn make_relayer(
    source: Arc<InMemoryDepositSource>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
    start_deposit_id: u64,
) -> R {
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

#[tokio::test]
async fn td_03_forged_block_hash_rejected_err_unknown_block() {
    let event = forged_deposit_event(0);
    let bundle = forged_bundle(&event);
    let submitter = anchor_gate_without_forged_hash();

    let outcome = submitter.submit(&event, &bundle).await.unwrap();
    match outcome {
        SubmitOutcome::Rejected { reason } => {
            assert!(
                reason.contains("ERR_UNKNOWN_BLOCK"),
                "TD-03: expected ERR_UNKNOWN_BLOCK hint: {reason}"
            );
            assert!(
                reason.contains(&format!("exit_code={EXIT_UNKNOWN_BLOCK}")),
                "TD-03: expected exit_code=224: {reason}"
            );
        }
        other => panic!("TD-03: forged blockHash must not finalize: {other:?}"),
    }
    assert_eq!(submitter.finalized_count(), 0);
}

#[tokio::test]
async fn td_03_forged_block_relayer_tick_no_finalize() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(forged_deposit_event(0));

    let submitter = Arc::new(anchor_gate_without_forged_hash());
    let mut relayer = make_relayer(
        source,
        submitter.clone(),
        dir.path().join("state.json"),
        0,
    );

    let outcome = relayer.tick().await.unwrap();
    match outcome {
        TickOutcome::AnRejected { deposit_id: 0, reason } => {
            assert!(reason.contains("ERR_UNKNOWN_BLOCK"));
            assert!(reason.contains("224"));
        }
        other => panic!("TD-03: relayer must not mint on unanchored block: {other:?}"),
    }
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

#[tokio::test]
async fn td_03_anchored_forged_block_hash_happy_path_submit() {
    let event = forged_deposit_event(1);
    let bundle = forged_bundle(&event);
    let mut accepted = HashSet::new();
    accepted.insert(FORGED_BLOCK_HASH);
    let submitter = MockAnSubmitter::with_anchor_gate(SEPOLIA, accepted);

    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![1]);
}

#[tokio::test]
async fn td_03_anchored_forged_block_relayer_tick_finalized() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(forged_deposit_event(2));

    let mut accepted = HashSet::new();
    accepted.insert(FORGED_BLOCK_HASH);
    let submitter = Arc::new(MockAnSubmitter::with_anchor_gate(SEPOLIA, accepted));
    let mut relayer = make_relayer(
        source,
        submitter.clone(),
        dir.path().join("state.json"),
        2,
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 2, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![2]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(2));
}
