//! TD-51 — Byzantine / split-brain RPC inconsistent responses per tick.
//!
//! Surrogate Byzantine `DepositSource` (pattern `f10_fault_injection` / `td_39`):
//! inconsistent head, chainId, or blockHash between sequential fetches must
//! fail-closed (`Err` at bind) or safe stall (`Ok(None)` / HOL), never silent
//! wrong finalize.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    error::RelayerError,
    is_deposit_block_finalized,
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::{DepositSource, InMemoryDepositSource},
    submitter::MockAnSubmitter,
    types::{DepositEvent, DepositProofBundle},
};
use tempfile::tempdir;

const SEPOLIA: u64 = 11_155_111;
const BASE: u64 = 8_453;
const CONFIRMATIONS: u64 = 12;

fn deposit(id: u64, source_chain_id: u64, block_number: u64, block_hash: B256) -> DepositEvent {
    DepositEvent {
        deposit_id: id,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64 + id),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64 + id),
        tx_hash: B256::repeat_byte(0xaa + id as u8),
        log_index: 0,
        block_number,
        block_hash,
        source_contract: Address::repeat_byte(0x22),
        source_chain_id,
    }
}

type R<S, P> = Relayer<S, P, MockAnSubmitter>;

fn relayer_cfg(state_path: PathBuf) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg
}

fn make_relayer<S, P>(
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
) -> R<S, P>
where
    S: DepositSource,
    P: ProofGenerator,
{
    Relayer::new(relayer_cfg(state_path), source, prover, submitter).unwrap()
}

/// Simulates `EthLogSource::fetch` head proxy: only surface deposit when
/// `block_number <= safe_head`.
struct HeadGatedSource {
    event: DepositEvent,
    heads: Vec<u64>,
    confirmations: u64,
    call_idx: Mutex<usize>,
}

impl HeadGatedSource {
    fn new(event: DepositEvent, heads: Vec<u64>, confirmations: u64) -> Self {
        Self {
            event,
            heads,
            confirmations,
            call_idx: Mutex::new(0),
        }
    }
}

#[async_trait]
impl DepositSource for HeadGatedSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        if deposit_id != self.event.deposit_id {
            return Ok(None);
        }
        let idx = {
            let mut guard = self.call_idx.lock().expect("poisoned");
            let i = *guard;
            *guard = i + 1;
            i
        };
        let head = self
            .heads
            .get(idx)
            .copied()
            .unwrap_or_else(|| self.heads.last().copied().unwrap_or(0));
        if !is_deposit_block_finalized(self.event.block_number, head, self.confirmations) {
            return Ok(None);
        }
        Ok(Some(self.event.clone()))
    }
}

/// Prover stamps PI `chainId` from a witness RPC (split-brain vs source event).
#[derive(Clone, Debug)]
struct SplitBrainProver {
    witness_chain_id: u64,
}

#[async_trait]
impl ProofGenerator for SplitBrainProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        let mut parsed = MockProofGenerator::derive_public_inputs(event, U256::ZERO);
        parsed.chain_id = U256::from(self.witness_chain_id);
        Ok(DepositProofBundle {
            vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
            public_inputs: parsed.to_operand().into(),
            proof: vec![0xde, 0xad].into(),
            parsed,
        })
    }
}

/// Prover binds receipt-derived `blockHash` while the event carries getLogs hash.
#[derive(Clone, Debug)]
struct ReceiptBlockHashProver {
    receipt_block_hash: B256,
}

#[async_trait]
impl ProofGenerator for ReceiptBlockHashProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        let mut parsed = MockProofGenerator::derive_public_inputs(event, U256::ZERO);
        let half = |slice: &[u8]| {
            let mut buf = [0u8; 32];
            buf[32 - slice.len()..].copy_from_slice(slice);
            U256::from_be_bytes::<32>(buf)
        };
        let rh = self.receipt_block_hash.as_slice();
        parsed.block_hash_high = half(&rh[0..16]);
        parsed.block_hash_low = half(&rh[16..32]);
        Ok(DepositProofBundle {
            vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
            public_inputs: parsed.to_operand().into(),
            proof: vec![0xde, 0xad].into(),
            parsed,
        })
    }
}

/// (a) Shrinking head: first fetch finalized, second tick head shrinks → stall (HOL).
#[tokio::test]
async fn td_51_a_shrinking_head_stalls_no_finalize() {
    let dir = tempdir().unwrap();
    let event = deposit(0, SEPOLIA, 100, B256::repeat_byte(0xcd));
    let source = Arc::new(HeadGatedSource::new(
        event,
        vec![112, 105], // tick1 safe=100 ok; tick2 safe=93, block 100 > 93 → None
        CONFIRMATIONS,
    ));
    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));

    // Deposit 1: shrunk head hides event — no erroneous finalize / cursor skip.
    let event1 = deposit(1, SEPOLIA, 101, B256::repeat_byte(0xce));
    let source2 = Arc::new(HeadGatedSource::new(
        event1,
        vec![105], // safe=93, block 101 not buried
        CONFIRMATIONS,
    ));
    let state_path2 = dir.path().join("state2.json");
    let mut seeded = deposit_relayer_daemon::state::RelayerState::default();
    seeded.record_progress(0);
    seeded.save(&state_path2).unwrap();
    let mut relayer2 = make_relayer(
        source2,
        Arc::new(MockProofGenerator::new()),
        Arc::new(MockAnSubmitter::accepting()),
        state_path2,
    );

    assert!(matches!(
        relayer2.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 1 }
    ));
    assert_eq!(relayer2.state().last_processed_deposit_id, Some(0));
}

/// (a) Pre-finalize stall: head too low on first tick, recovers when head grows.
#[tokio::test]
async fn td_51_a_shrinking_head_pre_finalize_stall_then_recover() {
    let dir = tempdir().unwrap();
    let event = deposit(0, SEPOLIA, 100, B256::repeat_byte(0xcd));
    let source = Arc::new(HeadGatedSource::new(
        event,
        vec![105, 112], // tick1 stall; tick2 finalize
        CONFIRMATIONS,
    ));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::NotYetAvailable { deposit_id: 0 }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, None);
    assert!(submitter.finalized_log().is_empty());

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}

/// (b) ChainId flip: event Sepolia, prover witness Base → bind reject before finalize.
#[tokio::test]
async fn td_51_b_chain_id_flip_tick_errors_before_finalize() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0, SEPOLIA, 100, B256::repeat_byte(0xcd)));

    let prover = Arc::new(SplitBrainProver {
        witness_chain_id: BASE,
    });
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state.json"),
    );

    let err = relayer.tick().await.unwrap_err().to_string();
    assert!(
        err.contains("chainId") && err.contains("eth_chainId"),
        "TD-51 chainId split-brain must fail closed: {err}"
    );
    assert!(submitter.finalized_log().is_empty());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

/// (b) Sequential source chainId flip: bind rejects second event vs first proof.
#[test]
fn td_51_b_sequential_chain_id_flip_rejects_bind() {
    let sep = deposit(0, SEPOLIA, 100, B256::repeat_byte(0xcd));
    let base = deposit(0, BASE, 100, B256::repeat_byte(0xcd));
    let bundle = MockProofGenerator::derive_public_inputs(&sep, U256::ZERO);
    let bundle = DepositProofBundle {
        vk_blob: vec![0x56, 0x4b].into(),
        public_inputs: bundle.to_operand().into(),
        proof: vec![0xaa].into(),
        parsed: bundle,
    };
    let err = bundle.check_binds_to(&base).unwrap_err().to_string();
    assert!(
        err.contains("chainId") && err.contains("eth_chainId"),
        "TD-51 flip Sepolia→Base bind: {err}"
    );
}

/// (c) getLogs `block_hash` ≠ receipt-derived hash → bind Err, no finalize.
#[tokio::test]
async fn td_51_c_block_hash_mismatch_errors_before_finalize() {
    let dir = tempdir().unwrap();
    let logs_hash = B256::repeat_byte(0xcd);
    let receipt_hash = B256::repeat_byte(0xee);
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0, SEPOLIA, 100, logs_hash));

    let prover = Arc::new(ReceiptBlockHashProver {
        receipt_block_hash: receipt_hash,
    });
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        prover,
        submitter.clone(),
        dir.path().join("state.json"),
    );

    let err = relayer.tick().await.unwrap_err().to_string();
    assert!(
        err.contains("blockHash"),
        "TD-51 blockHash split-brain must fail closed: {err}"
    );
    assert!(submitter.finalized_log().is_empty());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

/// (d) Control: consistent Byzantine-free source → Finalized on MockAn.
#[tokio::test]
async fn td_51_d_control_consistent_source_finalizes() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(0, SEPOLIA, 100, B256::repeat_byte(0xcd)));

    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
        dir.path().join("state.json"),
    );

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}
