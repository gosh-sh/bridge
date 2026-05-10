//! [`BridgeClient`] — abstraction over the on-chain
//! `AckiNackiBridge.verifyBlock` entry point.
//!
//! Two implementations:
//!
//! - [`EthBridgeClient`] — production. Wraps an ethers-rs
//!   `abigen!`-generated contract binding. Reads
//!   `storedLastSeenBlockSeqNo` / `storedBkSetCommitment` /
//!   `storedPrevMaxLevelLayerHash` from chain and submits
//!   `verifyBlock(...)` transactions.
//! - [`MockBridgeClient`] — a deterministic in-memory mirror of the
//!   contract's state machine, exposed to unit tests so we can drive the
//!   relayer through 5+ blocks in microseconds without spawning Anvil.
//!   The mock reproduces *exactly* the cheap pre-flight checks the real
//!   contract performs (numLayers range, tail zero, BK-set match,
//!   monotonic seqNo, anchor match) — so any consumer that passes the
//!   mock will also pass the real bridge unless ZK proofs are bad.
//!
//! ZK verification itself is *not* mocked here in the way the Solidity
//! `MockPrimaryVerifier` etc. mocks do; the [`MockBridgeClient`] takes
//! a `verifier_decision: Fn(&AnBlockData) -> bool` so tests can simulate
//! a verifier-rejection path explicitly.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use ethers::contract::Contract;
use ethers::prelude::*;
use ethers::types::{Address, Bytes, TransactionReceipt, U256};

use crate::error::RelayerError;
use crate::types::{AnBlockData, MAX_LAYER_HASHES};

// ─────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────

/// Snapshot of the on-chain anchors the relayer reads before deciding
/// what to submit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeOnChainState {
    pub last_seen_block_seq_no: u64,
    pub bk_set_commitment: U256,
    pub prev_max_level_layer_hash: U256,
}

/// Outcome of `submit_block`. The relayer interprets this to decide
/// whether to advance state, retry, or skip.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// `verifyBlock` succeeded; new on-chain anchors are reflected here.
    Verified {
        new_state: BridgeOnChainState,
        /// `Some` for the real bridge, `None` for the mock (no tx).
        tx_hash: Option<H256>,
    },
    /// `verifyBlock` reverted. The string carries the human-readable
    /// reason; the relayer doesn't try to parse selectors here (the
    /// Solidity custom errors map to a stable set of relayer reactions).
    Reverted { reason: String },
}

#[async_trait]
pub trait BridgeClient: Send + Sync {
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError>;
    async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// MockBridgeClient — in-memory mirror of AckiNackiBridge state machine
// ─────────────────────────────────────────────────────────────────────

/// Verifier decision callback.
///
/// `fn(&AnBlockData) -> bool` — returns `true` if both proofs would be
/// accepted on-chain (the real Groth16 verifiers verify field-element
/// public inputs against the proof bytes). Tests use this hook to
/// inject "rejected" outcomes deterministically.
pub type VerifierDecision = Arc<dyn Fn(&AnBlockData) -> bool + Send + Sync>;

/// In-memory bridge mirroring `AckiNackiBridge.verifyBlock` semantics.
pub struct MockBridgeClient {
    inner: Mutex<MockBridgeInner>,
    verifier: VerifierDecision,
}

struct MockBridgeInner {
    last_seen_block_seq_no: u64,
    bk_set_commitment: U256,
    num_layers: u8,
    layer_hashes: [U256; MAX_LAYER_HASHES],
    prev_max_level_layer_hash: U256,
    /// Receipt of every accepted block, in insertion order. Used by
    /// tests to assert the exact stream the relayer produced.
    accepted_log: Vec<AnBlockData>,
}

impl MockBridgeClient {
    /// Construct with the genesis anchors set by the bridge constructor.
    pub fn with_genesis(
        bk_set_commitment: U256,
        prev_max_level_layer_hash: U256,
        verifier: VerifierDecision,
    ) -> Self {
        Self {
            inner: Mutex::new(MockBridgeInner {
                last_seen_block_seq_no: 0,
                bk_set_commitment,
                num_layers: 0,
                layer_hashes: [U256::zero(); MAX_LAYER_HASHES],
                prev_max_level_layer_hash,
                accepted_log: Vec::new(),
            }),
            verifier,
        }
    }

    /// Test helper: assert how many blocks have been accepted to date.
    pub fn accepted_count(&self) -> usize {
        self.inner.lock().expect("poisoned lock").accepted_log.len()
    }

    /// Test helper: clone the accepted log.
    pub fn accepted_log(&self) -> Vec<AnBlockData> {
        self.inner.lock().expect("poisoned lock").accepted_log.clone()
    }
}

#[async_trait]
impl BridgeClient for MockBridgeClient {
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError> {
        let inner = self.inner.lock().expect("poisoned lock");
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: inner.last_seen_block_seq_no,
            bk_set_commitment: inner.bk_set_commitment,
            prev_max_level_layer_hash: inner.prev_max_level_layer_hash,
        })
    }

    async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError> {
        // Mirror the contract's checks, in order, with the same revert
        // strings the relayer would see from the live bridge.
        block.validate_shape()?;

        let mut inner = self.inner.lock().expect("poisoned lock");

        if block.bk_set_commitment != inner.bk_set_commitment {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "BkSetCommitmentMismatch(supplied={:#x}, stored={:#x})",
                    block.bk_set_commitment, inner.bk_set_commitment
                ),
            });
        }
        if block.block_seq_no <= inner.last_seen_block_seq_no {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "BlockSeqNoNotMonotonic(supplied={}, stored={})",
                    block.block_seq_no, inner.last_seen_block_seq_no
                ),
            });
        }
        if block.prev_max_level_layer_hash != inner.prev_max_level_layer_hash {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "PrevAnchorMismatch(supplied={:#x}, stored={:#x})",
                    block.prev_max_level_layer_hash, inner.prev_max_level_layer_hash
                ),
            });
        }

        if !(self.verifier)(block) {
            return Ok(SubmitOutcome::Reverted {
                reason: "AttestationProofRejected || LayerHashesProofRejected".to_string(),
            });
        }

        // Effects.
        inner.last_seen_block_seq_no = block.block_seq_no;
        inner.num_layers = block.num_layers;
        inner.layer_hashes = block.layer_hashes;
        inner.prev_max_level_layer_hash = block.next_anchor();
        inner.accepted_log.push(block.clone());

        Ok(SubmitOutcome::Verified {
            new_state: BridgeOnChainState {
                last_seen_block_seq_no: inner.last_seen_block_seq_no,
                bk_set_commitment: inner.bk_set_commitment,
                prev_max_level_layer_hash: inner.prev_max_level_layer_hash,
            },
            tx_hash: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────
// EthBridgeClient — production wrapper over ethers-rs abigen bindings
// ─────────────────────────────────────────────────────────────────────

abigen!(
    AckiNackiBridge,
    r#"[
        function verifyBlock(uint8 finType, bytes attestationProof, bytes layerHashesProof, uint256 blockId, uint256 bkSetCommitment, uint64 blockSeqNo, uint8 numLayers, uint256[10] layerHashes, uint256 prevMaxLevelLayerHash) external
        function storedLastSeenBlockSeqNo() external view returns (uint64)
        function storedBkSetCommitment() external view returns (uint256)
        function storedPrevMaxLevelLayerHash() external view returns (uint256)
        event BlockVerified(uint256 indexed blockId, uint64 indexed blockSeqNo, uint8 finType, uint8 numLayers)
    ]"#
);

/// Production bridge client — wraps `abigen!`-generated bindings.
///
/// Generic over any ethers `Middleware` (a signer-middleware in
/// production; `Provider<Http>` in read-only smoke tests).
pub struct EthBridgeClient<M: Middleware + 'static> {
    contract: AckiNackiBridge<M>,
    address: Address,
}

impl<M: Middleware + 'static> EthBridgeClient<M> {
    pub fn new(address: Address, client: Arc<M>) -> Self {
        let contract = AckiNackiBridge::new(address, client);
        Self { contract, address }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Direct access to the underlying contract (escape hatch for
    /// tests that want to attach event-stream subscriptions etc.).
    pub fn contract(&self) -> &AckiNackiBridge<M> {
        &self.contract
    }
}

#[async_trait]
impl<M: Middleware + 'static> BridgeClient for EthBridgeClient<M>
where
    M::Error: 'static,
{
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError> {
        let last = self
            .contract
            .stored_last_seen_block_seq_no()
            .call()
            .await
            .map_err(map_contract_err)?;
        let bk = self
            .contract
            .stored_bk_set_commitment()
            .call()
            .await
            .map_err(map_contract_err)?;
        let anchor = self
            .contract
            .stored_prev_max_level_layer_hash()
            .call()
            .await
            .map_err(map_contract_err)?;
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: last,
            bk_set_commitment: bk,
            prev_max_level_layer_hash: anchor,
        })
    }

    async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError> {
        block.validate_shape()?;

        let call = self.contract.verify_block(
            block.fin_type.tag(),
            block.attestation_proof.clone(),
            block.layer_hashes_proof.clone(),
            block.block_id,
            block.bk_set_commitment,
            block.block_seq_no,
            block.num_layers,
            block.layer_hashes,
            block.prev_max_level_layer_hash,
        );

        // Send-and-await with the borrow of `call` already dropped before
        // we re-borrow `self` for `read_state()`.
        let send_res = call.send().await;
        let outcome = match send_res {
            Ok(pending) => match pending.await {
                Ok(Some(receipt)) => Some(receipt.transaction_hash),
                Ok(None) => {
                    return Ok(SubmitOutcome::Reverted {
                        reason: "tx dropped from mempool without receipt".to_string(),
                    });
                }
                Err(e) => {
                    return Ok(SubmitOutcome::Reverted {
                        reason: format!("tx confirmation error: {e}"),
                    });
                }
            },
            Err(e) => {
                return Ok(SubmitOutcome::Reverted {
                    reason: format!("verifyBlock send failed: {e}"),
                });
            }
        };

        let new_state = self.read_state().await?;
        Ok(SubmitOutcome::Verified { new_state, tx_hash: outcome })
    }
}

fn map_contract_err<E: std::fmt::Display>(e: E) -> RelayerError {
    RelayerError::other(format!("contract call failed: {e}"))
}

// Suppress warnings from auto-generated abigen code that aren't relevant
// for this skeleton (tx receipt parsing helpers). The lints fire on
// downstream-untouched code paths.
#[allow(dead_code)]
fn _force_eth_types_in_scope(_b: Bytes, _t: Option<TransactionReceipt>, _c: Option<Contract<()>>) {}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FinalizationType;

    fn always_accept() -> VerifierDecision {
        Arc::new(|_| true)
    }

    fn block(seq: u64, prev_anchor: U256) -> AnBlockData {
        let mut layer_hashes = [U256::zero(); MAX_LAYER_HASHES];
        layer_hashes[0] = U256::from(seq * 100 + 1);
        AnBlockData {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            bk_set_commitment: U256::from(0xBE5E7u64),
            block_seq_no: seq,
            num_layers: 1,
            layer_hashes,
            prev_max_level_layer_hash: prev_anchor,
            attestation_proof: Bytes::from(vec![0u8; 32]),
            layer_hashes_proof: Bytes::from(vec![0u8; 32]),
        }
    }

    #[tokio::test]
    async fn mock_bridge_advances_state_through_three_blocks() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::zero(), always_accept());

        for seq in 1..=3 {
            let st = bridge.read_state().await.unwrap();
            let b = block(seq, st.prev_max_level_layer_hash);
            match bridge.submit_block(&b).await.unwrap() {
                SubmitOutcome::Verified { new_state, .. } => {
                    assert_eq!(new_state.last_seen_block_seq_no, seq);
                    assert_eq!(new_state.prev_max_level_layer_hash, b.next_anchor());
                }
                SubmitOutcome::Reverted { reason } => panic!("unexpected revert: {reason}"),
            }
        }
        assert_eq!(bridge.accepted_count(), 3);
    }

    #[tokio::test]
    async fn mock_bridge_reverts_on_seqno_replay() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::zero(), always_accept());
        bridge.submit_block(&block(1, U256::zero())).await.unwrap();
        let st = bridge.read_state().await.unwrap();
        let outcome = bridge
            .submit_block(&block(1, st.prev_max_level_layer_hash))
            .await
            .unwrap();
        match outcome {
            SubmitOutcome::Reverted { reason } => assert!(reason.contains("BlockSeqNoNotMonotonic")),
            _ => panic!("expected revert"),
        }
    }

    #[tokio::test]
    async fn mock_bridge_reverts_on_anchor_mismatch() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::zero(), always_accept());
        bridge.submit_block(&block(1, U256::zero())).await.unwrap();
        let outcome = bridge
            .submit_block(&block(2, U256::from(0xDEAD_BEEFu64)))
            .await
            .unwrap();
        match outcome {
            SubmitOutcome::Reverted { reason } => assert!(reason.contains("PrevAnchorMismatch")),
            _ => panic!("expected revert"),
        }
    }

    #[tokio::test]
    async fn mock_bridge_reverts_when_verifier_rejects() {
        let bridge = MockBridgeClient::with_genesis(
            U256::from(0xBE5E7u64),
            U256::zero(),
            Arc::new(|_| false),
        );
        let outcome = bridge.submit_block(&block(1, U256::zero())).await.unwrap();
        match outcome {
            SubmitOutcome::Reverted { reason } => {
                assert!(reason.contains("AttestationProofRejected") || reason.contains("LayerHashesProofRejected"))
            }
            _ => panic!("expected revert"),
        }
    }
}
