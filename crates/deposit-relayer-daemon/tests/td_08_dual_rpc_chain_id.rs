//! TD-08 — dual-RPC / `chainId` split-brain (source vs prover vs event PI).
//!
//! Catalog `TD-08`: source RPC stamps `event.source_chain_id` from its
//! `eth_chainId`, while a misconfigured prover subprocess (or witness RPC on
//! another network) would commit a different `chainId` in the twelve public
//! inputs. Expect fail-closed: `check_binds_to` reject or relayer error before
//! finalize — no mint with mismatched PI `chainId`.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use deposit_relayer_daemon::{
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
const MAINNET: u64 = 1;

fn deposit_on_chain(source_chain_id: u64) -> DepositEvent {
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
        source_chain_id: source_chain_id,
    }
}

/// Simulates a prover subprocess fed witness RPC on another L2/mainnet:
/// PI `chainId` comes from the prover's network, not the source log scanner.
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

type R<S, P> = Relayer<S, P, MockAnSubmitter>;

fn make_relayer<S, P>(
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<MockAnSubmitter>,
    state_path: PathBuf,
) -> R<S, P>
where
    S: deposit_relayer_daemon::source::DepositSource,
    P: ProofGenerator,
{
    let mut cfg = RelayerConfig::new(state_path);
    cfg.poll_interval = Duration::from_millis(0);
    cfg.max_attempts_warn = 16;
    Relayer::new(cfg, source, prover, submitter).unwrap()
}

fn td_08_bundle_with_witness_chain_id(event: &DepositEvent, witness_chain_id: u64) -> DepositProofBundle {
    let mut parsed = MockProofGenerator::derive_public_inputs(event, U256::ZERO);
    parsed.chain_id = U256::from(witness_chain_id);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde, 0xad].into(),
        parsed,
    }
}

#[test]
fn td_08_split_brain_pi_chain_id_mismatch_rejected_at_bind() {
    let event = deposit_on_chain(SEPOLIA);

    let err = td_08_bundle_with_witness_chain_id(&event, BASE)
        .check_binds_to(&event)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("chainId") && err.contains("eth_chainId"),
        "TD-08 Sepolia vs Base: {err}"
    );

    let err_main = td_08_bundle_with_witness_chain_id(&event, MAINNET)
        .check_binds_to(&event)
        .unwrap_err()
        .to_string();
    assert!(
        err_main.contains("chainId") && err_main.contains("eth_chainId"),
        "TD-08 Sepolia vs mainnet: {err_main}"
    );
}

#[tokio::test]
async fn td_08_split_brain_relayer_errors_before_finalize() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit_on_chain(SEPOLIA));

    let prover = Arc::new(SplitBrainProver {
        witness_chain_id: BASE,
    });
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(source, prover, submitter.clone(), dir.path().join("state.json"));

    let err = relayer.tick().await.unwrap_err().to_string();
    assert!(
        err.contains("chainId") && err.contains("eth_chainId"),
        "TD-08 relayer must fail closed on bind mismatch: {err}"
    );
    assert_eq!(submitter.finalized_log(), Vec::<u64>::new());
    assert_eq!(relayer.state().last_processed_deposit_id, None);
}

#[tokio::test]
async fn td_08_matching_source_and_prover_chain_id_happy_path() {
    let dir = tempdir().unwrap();
    let source = Arc::new(InMemoryDepositSource::new());
    source.insert(deposit_on_chain(SEPOLIA));

    let prover = Arc::new(MockProofGenerator::new());
    let submitter = Arc::new(MockAnSubmitter::accepting());
    let mut relayer = make_relayer(source, prover, submitter.clone(), dir.path().join("state.json"));

    assert!(matches!(
        relayer.tick().await.unwrap(),
        TickOutcome::Finalized { deposit_id: 0, .. }
    ));
    assert_eq!(submitter.finalized_log(), vec![0]);
    assert_eq!(relayer.state().last_processed_deposit_id, Some(0));
}
