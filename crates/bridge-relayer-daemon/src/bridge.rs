//! [`BridgeClient`] — abstraction over the on-chain
//! `AckiNackiBridge.verifyBlock` entry point.
//!
//! Two implementations:
//!
//! - [`EthBridgeClient`] — production. Wraps an alloy-rs `sol!`-generated
//!   contract binding. Reads `storedLastSeenBlockSeqNo` /
//!   `storedBkSetCommitment` (mutable) and `expectedPrevAnchor(numLayers)`
//!   (per-layer anchor pick) from chain, plus the immutable
//!   `storedPrevMaxLevelLayerHash` genesis seed, and submits
//!   `verifyBlock(...)` transactions. `storedPrevMaxLevelLayerHash` is
//!   the storage v2.0 immutable genesis seed (2026-08-04) — never
//!   mutated post-deploy; use `expectedPrevAnchor` for the actual chain
//!   anchor going forward.
//! - [`MockBridgeClient`] — a deterministic in-memory mirror of the contract's
//!   state machine, exposed to unit tests so we can drive the relayer through
//!   5+ blocks in microseconds without spawning Anvil. The mock reproduces
//!   *exactly* the cheap pre-flight checks the real contract performs
//!   (numLayers range, tail zero, BK-set match, monotonic seqNo, anchor match)
//!   — so any consumer that passes the mock will also pass the real bridge
//!   unless ZK proofs are bad. Under storage v2.0 (2026-08-04) the mock
//!   tracks per-layer window heads and implements `expectedPrevAnchor` with
//!   the same `min(numLayers, highestActiveLayer)` pick used on-chain.
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

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::{
    contract::Error as AlloyContractError,
    eips::BlockId,
    network::{Network, ReceiptResponse},
    primitives::{Address, B256, U256},
    providers::Provider,
};
use async_trait::async_trait;
// `EthBridgeContractState` and `HistoryWindow` are the alloy-neutral shape shared
// with `bridge_prover_lib::bridge_state::BridgeState::from_contract` — the
// relayer's `read_full_state` populates them (with BE→LE reversal on all
// `uint256` scalars) so consumers see a single unified byte order.
use bridge_prover_lib::bridge_state::{EthBridgeContractState, HistoryWindow};

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
    /// Storage v2.0 (2026-08-04): mirrors the on-chain **immutable**
    /// `storedPrevMaxLevelLayerHash()` getter — a constant genesis seed
    /// set by the constructor, never mutated by `verifyBlock`. This
    /// field is retained for backward compatibility with persisted
    /// relayer state files and for indexers that inspect the historical
    /// commitment. Callers doing a pre-submit drift check must use
    /// [`BridgeClient::expected_prev_anchor`] instead — the runtime
    /// anchor lives in per-layer rolling windows now.
    pub prev_max_level_layer_hash: U256,
    /// Highest seq_no applied via `applyBkSetUpdate` (0 if none yet).
    #[serde(default)]
    pub last_bk_set_update_seq_no: u64,
}

/// Width of the on-chain per-layer rolling window
/// (`HISTORY_PROOF_WINDOW` in `AckiNackiBridge.sol`).
/// Kept as a const here so the resurrect path fails loudly at compile
/// time if the contract-side constant is ever changed.
pub const HISTORY_PROOF_WINDOW: usize = 128;

// `MAX_LAYER_HASHES` (layer count = 10) is defined in `crate::types` and
// used here via the `use` at the top of the file — kept there as the
// single source of truth.

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

    /// Storage v2.0 (2026-08-04): the anchor a future
    /// `verifyBlock(.., numLayers, ..)` will require as
    /// `prevMaxLevelLayerHash`. Sourced from `_layerWindows` on-chain with
    /// the same `min(numLayers, highestActiveLayer)` per-layer pick the
    /// prover uses (`BridgeState::prev_max_level_layer_hash_for`). Callers
    /// must use this — not the immutable `storedPrevMaxLevelLayerHash`
    /// genesis seed exposed via [`BridgeOnChainState`] — for the pre-submit
    /// drift check.
    async fn expected_prev_anchor(&self, num_layers: u8) -> Result<U256, RelayerError>;
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
    /// Storage v2.0 (2026-08-04): immutable genesis seed set by
    /// [`MockBridgeClient::with_genesis`]. Corresponds to the on-chain
    /// `immutable storedPrevMaxLevelLayerHash`.
    genesis_prev_max_level_layer_hash: U256,
    /// Per-layer head (most recent append). Index `L-1` mirrors the
    /// contract's `_layerWindows[L]` head. Zero means "layer L never
    /// appended to". Fed by [`AnBlockData`] on every `submit_block`.
    latest_per_layer: [U256; MAX_LAYER_HASHES],
    /// Highest layer index (1-based) ever populated. Used by the
    /// per-layer anchor pick `min(numLayers, highestActiveLayer)`.
    highest_active_layer: u8,
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
                genesis_prev_max_level_layer_hash: prev_max_level_layer_hash,
                latest_per_layer: [U256::ZERO; MAX_LAYER_HASHES],
                highest_active_layer: 0,
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

impl MockBridgeInner {
    /// Mirror the on-chain `expectedPrevAnchor(numLayers)`:
    /// `pick = min(numLayers, highestActiveLayer)`, return
    /// `latest_per_layer[pick - 1]` if `pick > 0`, else the immutable
    /// genesis seed.
    fn expected_prev_anchor(&self, num_layers: u8) -> U256 {
        let pick = num_layers.min(self.highest_active_layer);
        if pick == 0 {
            self.genesis_prev_max_level_layer_hash
        } else {
            self.latest_per_layer[(pick - 1) as usize]
        }
    }
}

#[async_trait]
impl BridgeClient for MockBridgeClient {
    async fn read_state(&self) -> Result<BridgeOnChainState, RelayerError> {
        let inner = self.inner.lock().expect("poisoned lock");
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: inner.last_seen_block_seq_no,
            bk_set_commitment: inner.bk_set_commitment,
            // Storage v2.0: `prev_max_level_layer_hash` is the *immutable
            // genesis seed* mirror of `storedPrevMaxLevelLayerHash()`.
            // For the per-layer anchor query used by the pre-submit drift
            // check, call [`BridgeClient::expected_prev_anchor`].
            prev_max_level_layer_hash: inner.genesis_prev_max_level_layer_hash,
            last_bk_set_update_seq_no: inner.last_bk_set_update_seq_no,
        })
    }

    async fn expected_prev_anchor(&self, num_layers: u8) -> Result<U256, RelayerError> {
        let inner = self.inner.lock().expect("poisoned lock");
        Ok(inner.expected_prev_anchor(num_layers))
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
        // Storage v2.0: prev-anchor pick mirrors `expectedPrevAnchor(num_layers)`.
        let expected = inner.expected_prev_anchor(block.num_layers);
        if block.prev_max_level_layer_hash != expected {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "PrevAnchorMismatch(supplied={:#x}, expected={:#x})",
                    block.prev_max_level_layer_hash, expected
                ),
            });
        }

        if !(self.verifier)(block) {
            return Ok(SubmitOutcome::Reverted {
                reason: "AttestationProofRejected || LayerHashesProofRejected".to_string(),
            });
        }

        // Effects: mirror `_appendLayerHashes` — for L=1..=num_layers,
        // overwrite the per-layer head with the incoming hash (skipping
        // zero entries, like the contract does).
        inner.last_seen_block_seq_no = block.block_seq_no;
        for i in 0..block.num_layers {
            let h = block.layer_hashes[i as usize];
            if h != U256::ZERO {
                inner.latest_per_layer[i as usize] = h;
            }
        }
        if block.num_layers > inner.highest_active_layer {
            inner.highest_active_layer = block.num_layers;
        }
        inner.accepted_log.push(block.clone());

        Ok(SubmitOutcome::Verified {
            new_state: BridgeOnChainState {
                last_seen_block_seq_no: inner.last_seen_block_seq_no,
                bk_set_commitment: inner.bk_set_commitment,
                prev_max_level_layer_hash: inner.genesis_prev_max_level_layer_hash,
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
                prev_max_level_layer_hash: inner.genesis_prev_max_level_layer_hash,
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
            /// Storage v2.0 (2026-08-04): immutable genesis seed. Retained
            /// so historical indexers reading the constructor value keep
            /// working. Use `expectedPrevAnchor(numLayers)` for the anchor
            /// query and `getLatestPerLayer()` for per-layer state.
            function storedPrevMaxLevelLayerHash() external view returns (uint256);

            /// The chain anchor a future `verifyBlock(.., numLayers, ..)`
            /// will require as `prevMaxLevelLayerHash`. Sourced from the
            /// per-layer rolling windows (`_layerWindows`) with the same
            /// `min(numLayers, highestActiveLayer)` pick the prover uses
            /// (`prev_max_level_layer_hash_for`).
            function expectedPrevAnchor(uint8 numLayers) external view returns (uint256);

            /// Storage v2.0 (2026-08-04): replaces the removed
            /// `getStoredLayerHashes()`. Entry `[L-1]` is the most recent
            /// Poseidon Merkle root appended to layer `L` across all
            /// `verifyBlock` calls so far — not just the last block's
            /// array. Empty windows return zero.
            function getLatestPerLayer() external view returns (uint256[10] memory);

            /// Full contents of `_layerWindows[L]` — data + heights +
            /// dataLen + writeCursor + lastHeight. Off-chain-only reader
            /// used by the relayer daemon to reconstruct its BridgeState
            /// mirror during Case 6 chain-resurrect (see runbook).
            /// Added 2026-08.
            ///
            /// `HISTORY_PROOF_WINDOW` is fixed at 128 in `AckiNackiBridge.sol`;
            /// the array widths below are compile-time constants of the
            /// binding.
            struct HistoryWindow {
                uint256[128] data;
                uint64[128] heights;
                uint16 dataLen;
                uint16 writeCursor;
                uint64 lastHeight;
            }

            function getLayerWindow(uint8 layer) external view returns (HistoryWindow memory);

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

            /// Post-submit verification: is `anchor` present in layer `L`'s
            /// rolling `_layerWindows[L]` buffer? Called after `verifyBlock`
            /// to confirm the layer-hash append side-effect actually landed.
            function isKnownLayerAnchor(uint8 layer, uint256 anchor) external view returns (bool);

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

    /// Read the four top-level anchor slots pinned to a specific block.
    ///
    /// The trait's [`BridgeClient::read_state`] reads at `"latest"`, which
    /// can race behind a load-balanced public RPC (backend A gives us the
    /// receipt for block N; backend B still on block N-1 answers the
    /// follow-up eth_call). After a successful `verifyBlock` receipt we
    /// pin the reads to `receipt.block_number` so Tier 1 drift checks
    /// cannot false-fire on that lag.
    async fn read_state_at(&self, at: BlockId) -> Result<BridgeOnChainState, RelayerError> {
        let last = self
            .contract
            .storedLastSeenBlockSeqNo()
            .block(at)
            .call()
            .await
            .map_err(map_contract_err)?;
        let bk = self
            .contract
            .storedBkSetCommitment()
            .block(at)
            .call()
            .await
            .map_err(map_contract_err)?;
        let anchor = self
            .contract
            .storedPrevMaxLevelLayerHash()
            .block(at)
            .call()
            .await
            .map_err(map_contract_err)?;
        let last_bk = self
            .contract
            .storedLastBkSetUpdateSeqNo()
            .block(at)
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

    /// Read every field the daemon needs to reconstruct `BridgeState`
    /// from an already-advanced contract (Case 6 chain-resurrect).
    ///
    /// Issues 4 scalar view calls + 10 `getLayerWindow` calls. Each
    /// window returns ~5 KB, so the total off-chain cost is ~50 KB of
    /// RPC response — well under any provider's per-call cap, but the
    /// full read is worth pinning to a single block via a follow-up
    /// `read_full_state_at(BlockId)` if two consecutive `verifyBlock`
    /// receipts could interleave (not the case at daemon startup, hence
    /// the "latest block" call here).
    ///
    /// Consistency: no lock across calls, so a `verifyBlock` landing
    /// mid-read could cause the layer windows to be one block ahead of
    /// the scalar seq_no. That's fine for resurrect because we exit
    /// this call and let the normal startup drift routing re-observe on
    /// the next cycle — a one-block skew triggers an immediate re-read,
    /// not a mis-seed.
    pub async fn read_full_state(&self) -> Result<EthBridgeContractState, RelayerError> {
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
        let last_bk = self
            .contract
            .storedLastBkSetUpdateSeqNo()
            .call()
            .await
            .map_err(map_contract_err)?;
        let anchor = self
            .contract
            .storedPrevMaxLevelLayerHash()
            .call()
            .await
            .map_err(map_contract_err)?;

        // Read all 10 layer windows.
        // `HistoryWindow` on the sol! side has fixed-size arrays that
        // alloy exposes as `FixedBytes<32>[128]` / `u64[128]`.
        //
        // Endianness: on-chain `uint256` slots come out BE via
        // `U256::to_be_bytes`; `BridgeState.layer_windows` stores LE
        // (`Fr::to_repr()`). We reverse each slot here so the returned
        // `EthBridgeContractState` is byte-for-byte comparable to a local
        // `BridgeState` snapshot. Same convention applies to the two scalar
        // `uint256` fields (`bk_set_commitment`, `prev_max_level_layer_hash`).
        let mut windows: Vec<HistoryWindow> = Vec::with_capacity(MAX_LAYER_HASHES);
        for layer in 1..=MAX_LAYER_HASHES as u8 {
            let w = self
                .contract
                .getLayerWindow(layer)
                .call()
                .await
                .map_err(map_contract_err)?;
            let data: Vec<[u8; 32]> = w
                .data
                .iter()
                .map(|u| {
                    let mut le = u.to_be_bytes::<32>();
                    le.reverse();
                    le
                })
                .collect();
            let heights: Vec<u64> = w.heights.to_vec();
            // Widen on-chain `uint16` cursors to `usize` for the shared
            // `HistoryWindow` shape. `from_contract` validates bounds.
            windows.push(HistoryWindow {
                data,
                heights,
                data_len: w.dataLen as usize,
                write_cursor: w.writeCursor as usize,
                last_height: w.lastHeight,
            });
        }
        let layer_windows: [HistoryWindow; MAX_LAYER_HASHES] = windows
            .try_into()
            .map_err(|_| RelayerError::Other("read_full_state: expected 10 layer windows".into()))?;

        Ok(EthBridgeContractState {
            last_seen_block_seq_no: last,
            bk_set_commitment: bk.to_le_bytes::<32>(),
            last_bk_set_update_seq_no: last_bk,
            genesis_prev_max_level_layer_hash: anchor.to_le_bytes::<32>(),
            layer_windows,
        })
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

        // DEBUG: dump verifyBlock args to disk for offline replay via
        // `cast call` (env-gated so it never fires in production runs).
        //   BRIDGE_DUMP_SUBMISSIONS_DIR=./submissions cargo run … daemon-live
        // File format is a JSON object with hex-encoded proofs + numeric
        // uint256 anchors, directly consumable by an eth_call replay:
        //   cast call --rpc-url … <BRIDGE> "verifyBlock(...)" $(jq -r … dump.json)
        if let Ok(dir) = std::env::var("BRIDGE_DUMP_SUBMISSIONS_DIR") {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let fname = format!(
                "verifyBlock_seq{}_fin{}_{}.json",
                block.block_seq_no,
                block.fin_type.tag(),
                ts
            );
            let path = std::path::PathBuf::from(&dir).join(&fname);
            let layer_hashes: Vec<String> = block
                .layer_hashes
                .iter()
                .map(|h| format!("0x{:064x}", h))
                .collect();
            let payload = serde_json::json!({
                "bridge_address": format!("{:?}", self.contract.address()),
                "fin_type": block.fin_type.tag(),
                "attestation_proof_hex": format!("0x{}", hex::encode(&block.attestation_proof)),
                "layer_hashes_proof_hex": format!("0x{}", hex::encode(&block.layer_hashes_proof)),
                "block_id_uint256": format!("0x{:064x}", block.block_id),
                "bk_set_commitment_uint256": format!("0x{:064x}", block.bk_set_commitment),
                "block_seq_no": block.block_seq_no,
                "num_layers": block.num_layers,
                "layer_hashes_uint256": layer_hashes,
                "prev_max_level_layer_hash_uint256":
                    format!("0x{:064x}", block.prev_max_level_layer_hash),
            });
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("dump-submissions: mkdir {dir} failed: {e}");
            } else {
                match serde_json::to_string_pretty(&payload)
                    .map_err(|e| e.to_string())
                    .and_then(|s| std::fs::write(&path, s).map_err(|e| e.to_string()))
                {
                    Ok(()) => tracing::info!(
                        target: "bridge_relayer_daemon::bridge",
                        "dumped verifyBlock submission to {}",
                        path.display()
                    ),
                    Err(e) => tracing::warn!("dump-submissions: write {} failed: {e}", path.display()),
                }
            }
        }

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

        // Timeout on get_receipt: alloy's default is None (wait forever).
        // We cap at 180 s (~15 Sepolia blocks) so a stuck / dropped tx
        // surfaces as `SubmitOutcome::Reverted { reason: "timeout" }` and
        // counts toward `max_attempts_abort` instead of hanging the daemon.
        const RECEIPT_TIMEOUT: Duration = Duration::from_secs(180);

        let send_res = call.send().await;
        let (tx_hash, receipt_block) = match send_res {
            Ok(pending) => {
                let pending = pending.with_timeout(Some(RECEIPT_TIMEOUT));
                match pending.get_receipt().await {
                    Ok(receipt) => {
                        let bn = receipt.block_number().ok_or_else(|| {
                            RelayerError::other("verifyBlock receipt missing block_number")
                        })?;
                        (Some(receipt.transaction_hash()), bn)
                    },
                    Err(e) => {
                        return Ok(SubmitOutcome::Reverted {
                            reason: format!("tx confirmation error: {e}"),
                        });
                    },
                }
            },
            Err(e) => {
                return Ok(SubmitOutcome::Reverted {
                    reason: format!("verifyBlock send failed: {e}"),
                });
            },
        };

        // Pin all post-submit reads to the block that mined our tx. This
        // sidesteps read-after-write lag on load-balanced public RPCs
        // (see doc on `read_state_at`).
        let at = BlockId::from(receipt_block);
        let new_state = self.read_state_at(at).await?;

        // ---- Tier 1: top-level storage-slot drift -----------------------------
        // The receipt only proves the tx did not revert. Cross-check that the
        // three storage slots verifyBlock is documented to touch (see
        // AckiNackiBridge.sol:738/749 and the bkSetCommitment invariant) match
        // what we submitted. A mismatch here means the contract accepted the tx
        // but the on-chain state disagrees with our view of the block — treat
        // it as a revert so the daemon does not advance its cursor.
        if new_state.last_seen_block_seq_no != block.block_seq_no {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "post-submit drift: chain last_seen_block_seq_no={} != submitted={}",
                    new_state.last_seen_block_seq_no, block.block_seq_no
                ),
            });
        }
        // Storage v2.0 (2026-08-04): `storedPrevMaxLevelLayerHash` is now
        // the immutable genesis seed — the mutable "top layer of last block"
        // signal it used to expose is gone. Instead, verify the per-layer
        // rolling window head: after `verifyBlock(numLayers, layerHashes, ..)`
        // succeeds, `expectedPrevAnchor(numLayers)` must return
        // `layerHashes[numLayers - 1]` because the just-appended top-layer
        // hash is now the head of `_layerWindows[numLayers]` and the
        // per-layer pick with `pick == numLayers` returns exactly that. This
        // is a strictly stronger post-submit invariant than v1's flat
        // `storedPrevMaxLevelLayerHash` comparison.
        let expected_top = block.layer_hashes[(block.num_layers - 1) as usize];
        let post_anchor = self
            .contract
            .expectedPrevAnchor(block.num_layers)
            .block(at)
            .call()
            .await
            .map_err(map_contract_err)?;
        if post_anchor != expected_top {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "post-submit drift: expectedPrevAnchor({})={} != top layer of submitted block={}",
                    block.num_layers, post_anchor, expected_top
                ),
            });
        }
        if new_state.bk_set_commitment != block.bk_set_commitment {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "post-submit drift: chain bk_set_commitment={} != submitted={}",
                    new_state.bk_set_commitment, block.bk_set_commitment
                ),
            });
        }

        // ---- Tier 2: per-layer _layerWindows[L] append verification -----------
        // verifyBlock's _appendLayerHashes (AckiNackiBridge.sol:840) walks
        // L = 1..=numLayers and appends layerHashes[L-1] into _layerWindows[L]
        // iff the hash is non-zero. Confirm each non-zero layer hash we
        // submitted is now findable via isKnownLayerAnchor(L, hash).
        for i in 0..block.num_layers {
            let hash = block.layer_hashes[i as usize];
            if hash == U256::ZERO {
                // Contract skips zero-valued layer hashes, so don't assert
                // membership for them.
                continue;
            }
            let layer_idx: u8 = i + 1; // layers are 1-indexed in the contract
            let ok = self
                .contract
                .isKnownLayerAnchor(layer_idx, hash)
                .block(at)
                .call()
                .await
                .map_err(map_contract_err)?;
            if !ok {
                return Ok(SubmitOutcome::Reverted {
                    reason: format!(
                        "post-submit: layer {} hash {} not registered in _layerWindows \
                         (verifyBlock append side-effect missing)",
                        layer_idx, hash
                    ),
                });
            }
        }

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

    async fn expected_prev_anchor(&self, num_layers: u8) -> Result<U256, RelayerError> {
        self.contract
            .expectedPrevAnchor(num_layers)
            .call()
            .await
            .map_err(map_contract_err)
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
            // Storage v2.0: pull the per-layer anchor pick, not the
            // genesis-seed getter (`read_state` now returns the immutable
            // genesis for `prev_max_level_layer_hash`).
            let anchor = bridge.expected_prev_anchor(1).await.unwrap();
            let b = block(seq, anchor);
            match bridge.submit_block(&b).await.unwrap() {
                SubmitOutcome::Verified {
                    new_state, ..
                } => {
                    assert_eq!(new_state.last_seen_block_seq_no, seq);
                    // v2: read_state's `prev_max_level_layer_hash` is the
                    // immutable genesis (unchanged across blocks). The
                    // actual per-layer anchor lives behind
                    // `expected_prev_anchor(num_layers)`.
                    let expected = bridge.expected_prev_anchor(1).await.unwrap();
                    assert_eq!(expected, b.next_anchor());
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
        let anchor = bridge.expected_prev_anchor(1).await.unwrap();
        let outcome = bridge
            .submit_block(&block(1, anchor))
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
