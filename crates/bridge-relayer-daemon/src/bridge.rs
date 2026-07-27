//! [`BridgeClient`] — abstraction over the on-chain
//! `AckiNackiBridge.verifyBlock` entry point.
//!
//! Two implementations:
//!
//! - [`EthBridgeClient`] — production. Wraps an alloy-rs `sol!`-generated
//!   contract binding. Reads `storedLastSeenBlockSeqNo` /
//!   `storedBkSetCommitment` / `storedPrevMaxLevelLayerHash` from chain and
//!   submits `verifyBlock(...)` transactions.
//! - [`MockBridgeClient`] — a deterministic in-memory mirror of the contract's
//!   state machine, exposed to unit tests so we can drive the relayer through
//!   5+ blocks in microseconds without spawning Anvil. The mock reproduces
//!   *exactly* the cheap pre-flight checks the real contract performs
//!   (numLayers range, tail zero, BK-set match, monotonic seqNo, anchor match)
//!   — so any consumer that passes the mock will also pass the real bridge
//!   unless ZK proofs are bad.
//!
//! ZK verification itself is *not* mocked here in the way the Solidity
//! `MockPrimaryVerifier` etc. mocks do; the [`MockBridgeClient`] takes
//! a `verifier_decision: Fn(&AnBlockData) -> bool` so tests can simulate
//! a verifier-rejection path explicitly.
//!
//! ## Migration note (2026-05-17)
//!
//! Migrated from `ethers-rs 2.0.14` to `alloy 2.0.4`. The trait surface
//! ([`BridgeClient`]) is unchanged so callers (the relayer loop, the
//! mock-based unit tests) need no edits beyond the primitive type swap
//! `ethers::types::{U256, H256, Bytes}` → `alloy::primitives::{U256, B256,
//! Bytes}`. The production wrapper grew a tiny bit of generic plumbing to
//! abstract over the alloy [`Provider`] trait the same way it used to
//! abstract over ethers' `Middleware`.

use std::sync::{Arc, Mutex};

use alloy::{
    contract::Error as AlloyContractError,
    network::{Network, ReceiptResponse},
    primitives::{Address, B256, U256},
    providers::Provider,
};
use async_trait::async_trait;

use crate::{
    error::RelayerError,
    types::{AnBlockData, BkSetUpdateData, MAX_LAYER_HASHES},
    withdrawal::WithdrawalPublicInputs,
};

// ─────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────

/// Snapshot of the on-chain anchors the relayer reads before deciding
/// what to submit.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BridgeOnChainState {
    pub last_seen_block_seq_no: u64,
    pub bk_set_commitment: U256,
    pub prev_max_level_layer_hash: U256,
    /// Highest seq_no applied via `applyBkSetUpdate` (0 if none yet).
    #[serde(default)]
    pub last_bk_set_update_seq_no: u64,
}

/// Outcome of `submit_block`. The relayer interprets this to decide
/// whether to advance state, retry, or skip.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// `verifyBlock` succeeded; new on-chain anchors are reflected here.
    Verified {
        new_state: BridgeOnChainState,
        /// `Some` for the real bridge, `None` for the mock (no tx).
        tx_hash: Option<B256>,
    },
    /// `verifyBlock` reverted. The string carries the human-readable
    /// reason; the relayer doesn't try to parse selectors here (the
    /// Solidity custom errors map to a stable set of relayer reactions).
    Reverted { reason: String },
}

/// Outcome of [`EthBridgeClient::dry_run_block`] — a read-only
/// `eth_call` simulation of `verifyBlock(...)`. Used by the
/// `relayer verify-fixture` pre-flight check to confirm that a real
/// submit would not revert (including the ~287 k-gas Groth16 path
/// inside the verifier triple).
#[derive(Clone, Debug)]
pub enum DryRunOutcome {
    /// Simulation succeeded — a real submit at this point would verify
    /// (subject to no on-chain state change between now and the submit).
    WouldSucceed,
    /// Simulation reverted. `reason` is the raw alloy error chain so the
    /// operator can grep for custom-error selectors like
    /// `AttestationProofRejected` or `LayerHashesProofRejected`.
    WouldRevert { reason: String },
}

#[async_trait]
pub trait BridgeClient: Send + Sync {
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError>;
    async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError>;
    async fn submit_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError>;
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
    last_bk_set_update_seq_no: u64,
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
                last_bk_set_update_seq_no: 0,
                num_layers: 0,
                layer_hashes: [U256::ZERO; MAX_LAYER_HASHES],
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
        self.inner
            .lock()
            .expect("poisoned lock")
            .accepted_log
            .clone()
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
            last_bk_set_update_seq_no: inner.last_bk_set_update_seq_no,
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
                last_bk_set_update_seq_no: inner.last_bk_set_update_seq_no,
            },
            tx_hash: None,
        })
    }

    async fn submit_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned lock");
        if update.old_commitment_l2 != inner.bk_set_commitment {
            return Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!(
                    "BkSetCommitmentMismatch(supplied={:#x}, stored={:#x})",
                    update.old_commitment_l2, inner.bk_set_commitment
                ),
            });
        }
        if update.block_seq_no <= inner.last_bk_set_update_seq_no {
            return Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!(
                    "BkSetUpdateSeqNoNotMonotonic(supplied={}, stored={})",
                    update.block_seq_no, inner.last_bk_set_update_seq_no
                ),
            });
        }
        inner.bk_set_commitment = update.new_commitment_l3;
        inner.last_bk_set_update_seq_no = update.block_seq_no;
        Ok(BkSetUpdateSubmitOutcome::Applied {
            new_state: BridgeOnChainState {
                last_seen_block_seq_no: inner.last_seen_block_seq_no,
                bk_set_commitment: inner.bk_set_commitment,
                prev_max_level_layer_hash: inner.prev_max_level_layer_hash,
                last_bk_set_update_seq_no: inner.last_bk_set_update_seq_no,
            },
            tx_hash: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────
// EthBridgeClient — production wrapper over alloy sol! bindings
// ─────────────────────────────────────────────────────────────────────

// `sol!` expands into a `verifyBlock(...)` builder function with 10
// arguments — clippy's `too_many_arguments` lint trips on macro-generated
// code. Wrapping the macro invocation in a private module lets us scope
// the `allow` to just the generated bindings without polluting the rest
// of the file.
#[allow(clippy::too_many_arguments)]
mod sol_bindings {
    use alloy::sol;
    sol! {
        #[sol(rpc)]
        #[allow(missing_docs)]
        contract AckiNackiBridge {
            function verifyBlock(
                uint8 finType,
                bytes calldata attestationProof,
                bytes calldata layerHashesProof,
                uint256 blockId,
                uint256 bkSetCommitment,
                uint64 blockSeqNo,
                uint8 numLayers,
                uint256[10] calldata layerHashes,
                uint256 prevMaxLevelLayerHash
            ) external;

            function applyBkSetUpdate(
                uint8 finType,
                bytes calldata attestationProof,
                uint256 blockId,
                uint64 blockSeqNo,
                uint256 oldCommitmentL2,
                uint256 newCommitmentL3,
                bytes32 siblingH01,
                bytes32 siblingH4_7,
                bytes32 siblingH8_15
            ) external;

            function storedLastSeenBlockSeqNo() external view returns (uint64);
            function storedBkSetCommitment() external view returns (uint256);
            function storedLastBkSetUpdateSeqNo() external view returns (uint64);
            function storedPrevMaxLevelLayerHash() external view returns (uint256);

            struct WithdrawalPublicInputs {
                uint256 tokenId;
                uint256 amount;
                uint256 recipientHi;
                uint256 recipientLo;
                uint256 dstChainId;
                uint256 senderAccFr;
                uint256 dappFr;
                uint256 accFr;
                uint256 nullifier;
                uint256 finalRoot;
            }

            function withdrawByProof(
                bytes calldata proof,
                WithdrawalPublicInputs calldata pub
            ) external returns (bool success);

            function isNullifierUsed(uint256 nullifier) external view returns (bool);

            event BlockVerified(
                uint256 indexed blockId,
                uint64 indexed blockSeqNo,
                uint8 finType,
                uint8 numLayers
            );
        }
    }
}

use sol_bindings::AckiNackiBridge;

/// Production bridge client — wraps `sol!`-generated bindings.
///
/// Generic over any alloy [`Provider`] (a wallet-filled provider in
/// production; a plain HTTP provider in read-only smoke tests).
pub struct EthBridgeClient<P: Provider<N>, N: Network = alloy::network::Ethereum> {
    contract: AckiNackiBridge::AckiNackiBridgeInstance<P, N>,
    address: Address,
}

impl<P, N> EthBridgeClient<P, N>
where
    P: Provider<N> + Clone,
    N: Network,
{
    pub fn new(address: Address, provider: P) -> Self {
        let contract = AckiNackiBridge::new(address, provider);
        Self {
            contract,
            address,
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Direct access to the underlying contract (escape hatch for
    /// tests that want to attach event-stream subscriptions etc.).
    pub fn contract(&self) -> &AckiNackiBridge::AckiNackiBridgeInstance<P, N> {
        &self.contract
    }

    /// Simulate `verifyBlock(...)` via `eth_call` without sending a
    /// transaction. No gas spent, no signer required, no state mutated.
    /// Returns:
    ///
    /// - [`DryRunOutcome::WouldSucceed`] — every check the contract performs
    ///   (cheap pre-crypto + the three Groth16 verifiers) would accept the
    ///   inputs at the *current* on-chain state.
    /// - [`DryRunOutcome::WouldRevert`] — at least one check rejects; the alloy
    ///   error chain in `reason` typically carries the Solidity custom-error
    ///   selector and decoded args.
    ///
    /// This is what the `relayer verify-fixture` CLI uses to catch bad
    /// proofs as well as bad operator state. The cost on the operator's
    /// RPC quota is one `eth_call` per invocation — the verifier triple
    /// is fully executed, so on a free RPC this may take ~1 s per call.
    pub async fn dry_run_block(&self, block: &AnBlockData) -> Result<DryRunOutcome, RelayerError> {
        block.validate_shape()?;
        let call = self.contract.verifyBlock(
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
        match call.call().await {
            Ok(_) => Ok(DryRunOutcome::WouldSucceed),
            Err(e) => Ok(DryRunOutcome::WouldRevert {
                reason: format!("{e}"),
            }),
        }
    }

    /// Simulate `withdrawByProof(...)` via `eth_call`.
    pub async fn dry_run_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<DryRunOutcome, RelayerError> {
        let call = self
            .contract
            .withdrawByProof(proof.clone(), to_sol_withdrawal_pub(pub_inputs));
        match call.call().await {
            Ok(_) => Ok(DryRunOutcome::WouldSucceed),
            Err(e) => Ok(DryRunOutcome::WouldRevert {
                reason: format!("{e}"),
            }),
        }
    }

    /// Submit Circuit 4 `withdrawByProof` to Sepolia/mainnet.
    pub async fn submit_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<WithdrawSubmitOutcome, RelayerError> {
        let call = self
            .contract
            .withdrawByProof(proof.clone(), to_sol_withdrawal_pub(pub_inputs));
        match call.send().await {
            Ok(pending) => match pending.get_receipt().await {
                Ok(receipt) => Ok(WithdrawSubmitOutcome::Paid {
                    tx_hash: receipt.transaction_hash(),
                }),
                Err(e) => Ok(WithdrawSubmitOutcome::Reverted {
                    reason: format!("tx confirmation error: {e}"),
                }),
            },
            Err(e) => Ok(WithdrawSubmitOutcome::Reverted {
                reason: format!("withdrawByProof send failed: {e}"),
            }),
        }
    }

    /// Submit `applyBkSetUpdate` for a BK-set rotation bundle (`bkupd_*.json`).
    pub async fn submit_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        self.send_bk_set_update(update).await
    }

    async fn send_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        let call = self.contract.applyBkSetUpdate(
            update.fin_type.tag(),
            update.attestation_proof.clone(),
            update.block_id,
            update.block_seq_no,
            update.old_commitment_l2,
            update.new_commitment_l3,
            B256::from(update.sibling_h01),
            B256::from(update.sibling_h4_7),
            B256::from(update.sibling_h8_15),
        );
        match call.send().await {
            Ok(pending) => match pending.get_receipt().await {
                Ok(receipt) => {
                    // Inline read (avoid calling BridgeClient::read_state from
                    // an inherent method — that needs `P: 'static`).
                    let last = self
                        .contract
                        .storedLastSeenBlockSeqNo()
                        .call()
                        .await
                        .map_err(map_contract_err)?;
                    let bk = self
                        .contract
                        .storedBkSetCommitment()
                        .call()
                        .await
                        .map_err(map_contract_err)?;
                    let anchor = self
                        .contract
                        .storedPrevMaxLevelLayerHash()
                        .call()
                        .await
                        .map_err(map_contract_err)?;
                    let last_bk = self
                        .contract
                        .storedLastBkSetUpdateSeqNo()
                        .call()
                        .await
                        .map_err(map_contract_err)?;
                    Ok(BkSetUpdateSubmitOutcome::Applied {
                        new_state: BridgeOnChainState {
                            last_seen_block_seq_no: last,
                            bk_set_commitment: bk,
                            prev_max_level_layer_hash: anchor,
                            last_bk_set_update_seq_no: last_bk,
                        },
                        tx_hash: Some(receipt.transaction_hash()),
                    })
                }
                Err(e) => Ok(BkSetUpdateSubmitOutcome::Reverted {
                    reason: format!("tx confirmation error: {e}"),
                }),
            },
            Err(e) => Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!("applyBkSetUpdate send failed: {e}"),
            }),
        }
    }

    pub async fn is_nullifier_used(&self, nullifier: U256) -> Result<bool, RelayerError> {
        self.contract
            .isNullifierUsed(nullifier)
            .call()
            .await
            .map_err(map_contract_err)
    }
}

/// Outcome of [`EthBridgeClient::submit_withdraw`].
#[derive(Clone, Debug)]
pub enum WithdrawSubmitOutcome {
    Paid { tx_hash: B256 },
    Reverted { reason: String },
}

/// Outcome of [`EthBridgeClient::submit_bk_set_update`].
#[derive(Clone, Debug)]
pub enum BkSetUpdateSubmitOutcome {
    Applied {
        new_state: BridgeOnChainState,
        tx_hash: Option<B256>,
    },
    Reverted { reason: String },
}

fn to_sol_withdrawal_pub(
    pub_inputs: &WithdrawalPublicInputs,
) -> AckiNackiBridge::WithdrawalPublicInputs {
    AckiNackiBridge::WithdrawalPublicInputs {
        tokenId: pub_inputs.token_id,
        amount: pub_inputs.amount,
        recipientHi: pub_inputs.recipient_hi,
        recipientLo: pub_inputs.recipient_lo,
        dstChainId: pub_inputs.dst_chain_id,
        senderAccFr: pub_inputs.sender_acc_fr,
        dappFr: pub_inputs.dapp_fr,
        accFr: pub_inputs.acc_fr,
        nullifier: pub_inputs.nullifier,
        finalRoot: pub_inputs.final_root,
    }
}

#[async_trait]
impl<P, N> BridgeClient for EthBridgeClient<P, N>
where
    P: Provider<N> + Clone + Send + Sync + 'static,
    N: Network,
{
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError> {
        let last = self
            .contract
            .storedLastSeenBlockSeqNo()
            .call()
            .await
            .map_err(map_contract_err)?;
        let bk = self
            .contract
            .storedBkSetCommitment()
            .call()
            .await
            .map_err(map_contract_err)?;
        let anchor = self
            .contract
            .storedPrevMaxLevelLayerHash()
            .call()
            .await
            .map_err(map_contract_err)?;
        let last_bk = self
            .contract
            .storedLastBkSetUpdateSeqNo()
            .call()
            .await
            .map_err(map_contract_err)?;
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: last,
            bk_set_commitment: bk,
            prev_max_level_layer_hash: anchor,
            last_bk_set_update_seq_no: last_bk,
        })
    }

    async fn submit_block(&self, block: &AnBlockData) -> Result<SubmitOutcome, RelayerError> {
        block.validate_shape()?;

        let call = self.contract.verifyBlock(
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

        let send_res = call.send().await;
        let tx_hash = match send_res {
            Ok(pending) => match pending.get_receipt().await {
                Ok(receipt) => Some(receipt.transaction_hash()),
                Err(e) => {
                    return Ok(SubmitOutcome::Reverted {
                        reason: format!("tx confirmation error: {e}"),
                    });
                },
            },
            Err(e) => {
                return Ok(SubmitOutcome::Reverted {
                    reason: format!("verifyBlock send failed: {e}"),
                });
            },
        };

        let new_state = self.read_state().await?;
        Ok(SubmitOutcome::Verified {
            new_state,
            tx_hash,
        })
    }

    async fn submit_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        self.send_bk_set_update(update).await
    }
}

fn map_contract_err(e: AlloyContractError) -> RelayerError {
    RelayerError::other(format!("contract call failed: {e}"))
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use alloy::primitives::Bytes;

    use super::*;
    use crate::types::FinalizationType;

    fn always_accept() -> VerifierDecision {
        Arc::new(|_| true)
    }

    fn block(seq: u64, prev_anchor: U256) -> AnBlockData {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
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
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::ZERO, always_accept());

        for seq in 1..=3 {
            let st = bridge.read_state().await.unwrap();
            let b = block(seq, st.prev_max_level_layer_hash);
            match bridge.submit_block(&b).await.unwrap() {
                SubmitOutcome::Verified {
                    new_state, ..
                } => {
                    assert_eq!(new_state.last_seen_block_seq_no, seq);
                    assert_eq!(new_state.prev_max_level_layer_hash, b.next_anchor());
                },
                SubmitOutcome::Reverted {
                    reason,
                } => panic!("unexpected revert: {reason}"),
            }
        }
        assert_eq!(bridge.accepted_count(), 3);
    }

    #[tokio::test]
    async fn mock_bridge_reverts_on_seqno_replay() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::ZERO, always_accept());
        bridge.submit_block(&block(1, U256::ZERO)).await.unwrap();
        let st = bridge.read_state().await.unwrap();
        let outcome = bridge
            .submit_block(&block(1, st.prev_max_level_layer_hash))
            .await
            .unwrap();
        match outcome {
            SubmitOutcome::Reverted {
                reason,
            } => assert!(reason.contains("BlockSeqNoNotMonotonic")),
            _ => panic!("expected revert"),
        }
    }

    #[tokio::test]
    async fn mock_bridge_reverts_on_anchor_mismatch() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::ZERO, always_accept());
        bridge.submit_block(&block(1, U256::ZERO)).await.unwrap();
        let outcome = bridge
            .submit_block(&block(2, U256::from(0xDEAD_BEEFu64)))
            .await
            .unwrap();
        match outcome {
            SubmitOutcome::Reverted {
                reason,
            } => assert!(reason.contains("PrevAnchorMismatch")),
            _ => panic!("expected revert"),
        }
    }

    #[tokio::test]
    async fn mock_bridge_reverts_when_verifier_rejects() {
        let bridge =
            MockBridgeClient::with_genesis(U256::from(0xBE5E7u64), U256::ZERO, Arc::new(|_| false));
        let outcome = bridge.submit_block(&block(1, U256::ZERO)).await.unwrap();
        match outcome {
            SubmitOutcome::Reverted {
                reason,
            } => {
                assert!(
                    reason.contains("AttestationProofRejected")
                        || reason.contains("LayerHashesProofRejected")
                )
            },
            _ => panic!("expected revert"),
        }
    }
}
