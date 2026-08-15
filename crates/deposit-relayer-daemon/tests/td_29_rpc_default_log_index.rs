//! TD-29 — RPC `unwrap_or_default` on `log_index` / `block_hash` (Opus D-11).
//!
//! Documents QC binding/HOL risk when `eth_getLogs` omits metadata fields.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Log;
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    prover::MockProofGenerator,
    receipt_log_index_from_block_log,
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::DepositSource,
    submitter::MockAnSubmitter,
    types::{DepositEvent, DepositProofBundle},
};
use serde::Deserialize;
use tempfile::tempdir;

const SEPOLIA: u64 = 11_155_111;
const CANONICAL_BLOCK_HASH: B256 = B256::repeat_byte(0xcd);

#[derive(Debug, Deserialize)]
struct SepoliaFixture {
    block_log_index: u64,
    expected_receipt_log_index: u64,
    receipt_logs: Vec<Log>,
}

fn load_sepolia_fixture() -> SepoliaFixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sepolia_deposit_id0.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn deposit(id: u64, block_hash: B256, log_index: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_100u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index,
        block_number: 100 + id,
        block_hash,
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: SEPOLIA,
    }
}

struct FixedDepositSource {
    event: DepositEvent,
}

#[async_trait]
impl DepositSource for FixedDepositSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        Ok(if deposit_id == self.event.deposit_id {
            Some(self.event.clone())
        } else {
            None
        })
    }
}

fn relayer_cfg(state_path: PathBuf) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg
}

/// (A) Missing `logIndex` → `unwrap_or_default()` = 0; Sepolia receipt has no log with index 0.
#[test]
fn td_29_missing_log_index_default_zero_mapping_fails() {
    let fixture = load_sepolia_fixture();
    let block_log_index = 0u64; // decoded.log_index.unwrap_or_default()
    let err = receipt_log_index_from_block_log(&fixture.receipt_logs, block_log_index).unwrap_err();
    assert!(matches!(err, RelayerError::Eth(_)));
    assert!(err.to_string().contains("not found"));
}

/// Missing `logIndex` default collides with receipt position 0 (wrong log for prover).
#[test]
fn td_29_missing_log_index_zero_wrong_receipt_position_qc() {
    let fixture = load_sepolia_fixture();
    let mut logs = fixture.receipt_logs.clone();
    logs[0].log_index = Some(0);

    let mapped = receipt_log_index_from_block_log(&logs, 0).expect("index 0 present after default");
    assert_eq!(mapped, 0, "defaults to first receipt log");
    assert_ne!(
        mapped,
        fixture.expected_receipt_log_index,
        "TD-29 QC: prover would index wrong receipt log"
    );
}

/// (B) Missing `blockHash` → `unwrap_or_default()` = ZERO; anchor gate rejects (no wrong mint).
#[tokio::test]
async fn td_29_missing_block_hash_default_zero_anchor_rejects() {
    let dir = tempdir().unwrap();
    let event = deposit(0, B256::ZERO, 0);
    let source = Arc::new(FixedDepositSource {
        event: event.clone(),
    });
    let submitter = Arc::new(
        MockAnSubmitter::with_anchor_gate(SEPOLIA, HashSet::from([CANONICAL_BLOCK_HASH])),
    );
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json")),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnRejected { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_count(), 0);
}

/// Both defaults — ambiguous event; still no finalize under anchor gate.
#[tokio::test]
async fn td_29_both_defaults_ambiguous_no_finalize() {
    let dir = tempdir().unwrap();
    let event = deposit(0, B256::ZERO, 0);
    let source = Arc::new(FixedDepositSource {
        event: event.clone(),
    });
    let submitter = Arc::new(
        MockAnSubmitter::with_anchor_gate(SEPOLIA, HashSet::from([CANONICAL_BLOCK_HASH])),
    );
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json")),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter,
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::AnRejected { deposit_id: 0, .. }
    ));
}

/// Proof bound to canonical hash rejects event with default-zero `block_hash`.
#[test]
fn td_29_missing_block_hash_proof_bind_mismatch() {
    let canonical = deposit(0, CANONICAL_BLOCK_HASH, 2);
    let mut rpc_default = canonical.clone();
    rpc_default.block_hash = B256::ZERO;

    let bundle = DepositProofBundle {
        vk_blob: vec![0x56, 0x4b].into(),
        public_inputs: MockProofGenerator::derive_public_inputs(&canonical, U256::ZERO)
            .to_operand()
            .into(),
        proof: vec![0xaa].into(),
        parsed: MockProofGenerator::derive_public_inputs(&canonical, U256::ZERO),
    };

    let err = bundle.check_binds_to(&rpc_default).unwrap_err();
    assert!(matches!(err, RelayerError::ProofGeneration(_)));
    assert!(err.to_string().contains("blockHash"));
}

/// Control — full RPC fields map to correct receipt-local index.
#[test]
fn td_29_control_full_fields_correct_receipt_log_index() {
    let fixture = load_sepolia_fixture();
    let mapped = receipt_log_index_from_block_log(&fixture.receipt_logs, fixture.block_log_index)
        .expect("canonical mapping");
    assert_eq!(mapped, fixture.expected_receipt_log_index);
}
