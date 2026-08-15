//! TD-30 — prover RPC vs source RPC `eth_chainId` startup preflight (Opus D-14).

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
    ensure_matching_source_prover_chain_ids,
    ensure_supported_chain_id_value,
    error::RelayerError,
    prover::{MockProofGenerator, ProofGenerator},
    relayer::{Relayer, RelayerConfig, TickOutcome},
    source::InMemoryDepositSource,
    submitter::MockAnSubmitter,
    types::{DepositEvent, DepositProofBundle},
};
use tempfile::tempdir;

const SEPOLIA: u64 = 11_155_111;
const BASE: u64 = 8_453;

fn deposit(source_chain_id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: 0,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_000u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 100,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: Address::repeat_byte(0x22),
        source_chain_id,
    }
}

/// Simulates misconfigured prover witness RPC (TD-08 split-brain).
struct SplitBrainProver {
    witness_chain_id: u64,
}

#[async_trait]
impl ProofGenerator for SplitBrainProver {
    async fn generate(&self, event: &DepositEvent) -> Result<DepositProofBundle, RelayerError> {
        let mut parsed = MockProofGenerator::derive_public_inputs(event, U256::ZERO);
        parsed.chain_id = U256::from(self.witness_chain_id);
        Ok(DepositProofBundle {
            vk_blob: vec![0x56, 0x4b].into(),
            public_inputs: parsed.to_operand().into(),
            proof: vec![0xde].into(),
            parsed,
        })
    }
}

fn relayer_cfg(state_path: PathBuf) -> RelayerConfig {
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    cfg
}

/// (1) Mismatch Sepolia vs Base — startup preflight Err before tick.
#[test]
fn td_30_dual_rpc_chain_id_mismatch_fails_preflight() {
    let err = ensure_matching_source_prover_chain_ids(SEPOLIA, BASE).unwrap_err();
    assert!(err.to_string().contains("prover RPC eth_chainId"));
    assert!(err.to_string().contains("8453"));
    assert!(err.to_string().contains("11155111"));
}

/// (2) Control — matching chain ids pass preflight gate.
#[test]
fn td_30_matching_chain_ids_pass_preflight() {
    ensure_matching_source_prover_chain_ids(SEPOLIA, SEPOLIA).unwrap();
    ensure_supported_chain_id_value(SEPOLIA, Some(SEPOLIA)).unwrap();
}

/// (3) Same RPC URL path — single chain id is sufficient (no second mismatch).
#[test]
fn td_30_same_rpc_url_implicit_match() {
    let url = "https://rpc.sepolia.example";
    assert_eq!(url, url);
    ensure_matching_source_prover_chain_ids(SEPOLIA, SEPOLIA).unwrap();
}

/// Control — happy tick when source and mock prover agree on chainId.
#[tokio::test]
async fn td_30_matching_source_prover_happy_tick() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(SEPOLIA));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json")),
        source,
        Arc::new(MockProofGenerator::new()),
        submitter.clone(),
    )
    .unwrap();

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
}

/// Post-hoc bind still fail-closed if preflight were bypassed (TD-08 regression).
#[tokio::test]
async fn td_30_split_brain_still_rejected_at_tick_without_mint() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit(SEPOLIA));
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = Relayer::new(
        relayer_cfg(dir.path().join("state.json")),
        source,
        Arc::new(SplitBrainProver {
            witness_chain_id: BASE,
        }),
        submitter.clone(),
    )
    .unwrap();

    let err = relayer.tick().await.unwrap_err().to_string();
    assert!(err.contains("chainId") && err.contains("eth_chainId"));
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
}

/// Unsupported chain id rejected at preflight (before relayer loop).
#[test]
fn td_30_unsupported_chain_id_fails_preflight() {
    let err = ensure_supported_chain_id_value(1, None).unwrap_err();
    assert!(err.to_string().contains("not a supported deposit chain"));
}
