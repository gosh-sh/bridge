//! [`BridgeClient`] — abstraction over the on-chain
//! `AckiNackiBridge.verifyBlock` entry point.
//!
//! Two implementations:
//!
//! - [`EthBridgeClient`] — production. Wraps an alloy-rs `sol!`-generated
//!   contract binding. Reads `storedLastSeenBlockSeqNo` /
//!   `storedBkSetCommitment` (mutable) and `expectedPrevAnchor(numLayers)`
//!   (per-layer anchor pick) from chain, plus the immutable
//!   `storedPrevMaxLevelLayerHash` genesis seed, and submits `verifyBlock(...)`
//!   transactions. `storedPrevMaxLevelLayerHash` is the storage v2.0 immutable
//!   genesis seed (2026-08-04) — never mutated post-deploy; use
//!   `expectedPrevAnchor` for the actual chain anchor going forward.
//! - [`MockBridgeClient`] — a deterministic in-memory mirror of the contract's
//!   state machine, exposed to unit tests so we can drive the relayer through
//!   5+ blocks in microseconds without spawning Anvil. The mock reproduces
//!   *exactly* the cheap pre-flight checks the real contract performs
//!   (numLayers range, tail zero, BK-set match, monotonic seqNo, anchor match)
//!   — so any consumer that passes the mock will also pass the real bridge
//!   unless ZK proofs are bad. Under storage v2.0 (2026-08-04) the mock tracks
//!   per-layer window heads and implements `expectedPrevAnchor` with the same
//!   `min(numLayers, highestActiveLayer)` pick used on-chain.
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
    sol_types::SolCall,
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

/// [`EthBridgeClient::read_full_state`] pins every read to one block; a
/// lagging backend behind a load-balanced RPC may answer "header not found"
/// for it, so the whole snapshot is read up to this many times, spaced
/// [`READ_FULL_STATE_RETRY_DELAY_MS`] apart: two delays span a Sepolia
/// slot, so a backend one block behind has caught up by the last read.
const READ_FULL_STATE_ATTEMPTS: u32 = 3;
const READ_FULL_STATE_RETRY_DELAY_MS: u64 = 6_000;

// ─────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────

/// Snapshot of the on-chain anchors the relayer reads before deciding
/// what to submit.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BridgeOnChainState {
    pub last_seen_block_seq_no: u64,
    pub bk_set_commitment: U256,
    /// Outgoing BK-set after `applyBkSetUpdate(N)`. `verifyBlock` accepts
    /// this for `blockSeqNo <= last_bk_set_update_seq_no`. Zero until the
    /// first rotation. `serde(default)` keeps old `state.json` readable.
    #[serde(default)]
    pub prev_bk_set_commitment: U256,
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

impl BridgeOnChainState {
    /// Same rule as `AckiNackiBridge._expectedBkSetFor`.
    pub fn expected_bk_set_for(&self, block_seq_no: u64) -> U256 {
        if self.last_bk_set_update_seq_no != 0 && block_seq_no <= self.last_bk_set_update_seq_no {
            self.prev_bk_set_commitment
        } else {
            self.bk_set_commitment
        }
    }
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
    /// [`Self::read_state`] with every slot read at one block, so a
    /// load-balanced RPC cannot answer the five calls from backends at
    /// different heights. Defaults to [`Self::read_state`] for clients with
    /// a single consistent view (the mock).
    async fn read_state_pinned(&self) -> Result<BridgeOnChainState, RelayerError> {
        self.read_state().await
    }
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

/// The subset of `EthBridgeClient` the withdraw-scan loop calls. Extracted
/// as its own trait (rather than added to `BridgeClient`) so tests can
/// exercise the classifier/park branch of `withdraw_scan_once` without
/// standing up a mock of the full verifyBlock state machine. Production
/// hits `EthBridgeClient` directly via the blanket impl below.
#[async_trait]
pub trait WithdrawBridge: Send + Sync {
    /// The bridge contract `withdrawByProof` is sent to; a proof that names
    /// another one (`dstBridge`) reverts `WrongDstBridge` there.
    fn bridge_address(&self) -> Address;
    async fn is_nullifier_used(&self, nullifier: U256) -> Result<bool, RelayerError>;
    async fn dry_run_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<DryRunOutcome, RelayerError>;
    async fn submit_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<WithdrawSubmitOutcome, RelayerError>;
}

#[async_trait]
impl<P, N> WithdrawBridge for EthBridgeClient<P, N>
where
    P: Provider<N> + Clone + Send + Sync + 'static,
    N: Network,
{
    fn bridge_address(&self) -> Address {
        self.address
    }

    async fn is_nullifier_used(&self, nullifier: U256) -> Result<bool, RelayerError> {
        EthBridgeClient::is_nullifier_used(self, nullifier).await
    }

    async fn dry_run_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<DryRunOutcome, RelayerError> {
        EthBridgeClient::dry_run_withdraw(self, proof, pub_inputs).await
    }

    async fn submit_withdraw(
        &self,
        proof: &alloy::primitives::Bytes,
        pub_inputs: &WithdrawalPublicInputs,
    ) -> Result<WithdrawSubmitOutcome, RelayerError> {
        EthBridgeClient::submit_withdraw(self, proof, pub_inputs).await
    }
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
    prev_bk_set_commitment: U256,
    last_bk_set_update_seq_no: u64,
    /// Mirrors the on-chain `immutable bundleStride`. Consumed by the
    /// bundle-aware `PrevSlotStillOwedForBundle` gate in
    /// [`BridgeClient::submit_bk_set_update`]. Must be strictly positive
    /// — the ordering suites that need a non-trivial window exercise it
    /// explicitly; the default of `1` keeps every other suite's
    /// consecutive-seqno semantics intact (each `verifyBlock` covers the
    /// previous rotation).
    bundle_stride: u64,
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
    /// Uses `bundle_stride = 1` — matches the test-helper default in
    /// `VerifyBlockConfigLib.with(...)`. Suites that need a non-trivial
    /// window (ordering tests exercising `PrevSlotStillOwedForBundle`)
    /// call [`MockBridgeClient::with_genesis_stride`].
    pub fn with_genesis(
        bk_set_commitment: U256,
        prev_max_level_layer_hash: U256,
        verifier: VerifierDecision,
    ) -> Self {
        Self::with_genesis_stride(bk_set_commitment, prev_max_level_layer_hash, 1, verifier)
    }

    /// Construct with an explicit bundle stride. Only the bk-update
    /// ordering suites need a non-trivial stride.
    pub fn with_genesis_stride(
        bk_set_commitment: U256,
        prev_max_level_layer_hash: U256,
        bundle_stride: u64,
        verifier: VerifierDecision,
    ) -> Self {
        Self {
            inner: Mutex::new(MockBridgeInner {
                last_seen_block_seq_no: 0,
                bk_set_commitment,
                prev_bk_set_commitment: U256::ZERO,
                last_bk_set_update_seq_no: 0,
                bundle_stride,
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

    /// Test helper: a rotation and a bundle cursor already on the contract,
    /// as after a receipt that arrived late.
    pub fn land_rotation_for_test(&self, last_seen: u64, last_bk: u64, new_commitment: U256) {
        let mut inner = self.inner.lock().expect("poisoned lock");
        inner.prev_bk_set_commitment = inner.bk_set_commitment;
        inner.bk_set_commitment = new_commitment;
        inner.last_bk_set_update_seq_no = last_bk;
        inner.last_seen_block_seq_no = last_seen;
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
            prev_bk_set_commitment: inner.prev_bk_set_commitment,
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

        let expected_bk = if inner.last_bk_set_update_seq_no != 0
            && block.block_seq_no <= inner.last_bk_set_update_seq_no
        {
            inner.prev_bk_set_commitment
        } else {
            inner.bk_set_commitment
        };
        if block.bk_set_commitment != expected_bk {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "BkSetCommitmentMismatch(supplied={:#x}, stored={:#x})",
                    block.bk_set_commitment, expected_bk
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

        let new_state = BridgeOnChainState {
            last_seen_block_seq_no: inner.last_seen_block_seq_no,
            bk_set_commitment: inner.bk_set_commitment,
            prev_bk_set_commitment: inner.prev_bk_set_commitment,
            prev_max_level_layer_hash: inner.genesis_prev_max_level_layer_hash,
            last_bk_set_update_seq_no: inner.last_bk_set_update_seq_no,
        };
        // Same post-submit check as `EthBridgeClient`: the current
        // commitment is the new set after `applyBkSetUpdate(N)`, so
        // compare against `_expectedBkSetFor`.
        if new_state.expected_bk_set_for(block.block_seq_no) != block.bk_set_commitment {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "post-submit drift: expected_bk_set_for({}) != submitted",
                    block.block_seq_no
                ),
            });
        }

        Ok(SubmitOutcome::Verified {
            new_state,
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
        if inner.last_bk_set_update_seq_no != 0 {
            let stride = inner.bundle_stride.max(1);
            let next_owed_boundary = ((inner.last_seen_block_seq_no / stride) + 1) * stride;
            if next_owed_boundary <= inner.last_bk_set_update_seq_no {
                return Ok(BkSetUpdateSubmitOutcome::Reverted {
                    reason: format!(
                        "PrevSlotStillOwedForBundle(last_bk_set_update={}, next_owed_boundary={})",
                        inner.last_bk_set_update_seq_no, next_owed_boundary
                    ),
                });
            }
        }
        if update.attestation_last_seen >= update.block_seq_no {
            return Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!(
                    "AttestationLastSeenNotBeforeSeqNo(last_seen={}, seq={})",
                    update.attestation_last_seen, update.block_seq_no
                ),
            });
        }
        inner.prev_bk_set_commitment = inner.bk_set_commitment;
        inner.bk_set_commitment = update.new_commitment_l3;
        inner.last_bk_set_update_seq_no = update.block_seq_no;
        Ok(BkSetUpdateSubmitOutcome::Applied {
            new_state: BridgeOnChainState {
                last_seen_block_seq_no: inner.last_seen_block_seq_no,
                bk_set_commitment: inner.bk_set_commitment,
                prev_bk_set_commitment: inner.prev_bk_set_commitment,
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
                uint64 attestationLastSeen,
                uint256 oldCommitmentL2,
                uint256 newCommitmentL3,
                bytes32 siblingH01,
                bytes32 siblingH4_7,
                bytes32 siblingH8_15
            ) external;

            function storedLastSeenBlockSeqNo() external view returns (uint64);
            function storedBkSetCommitment() external view returns (uint256);
            function storedPrevBkSetCommitment() external view returns (uint256);
            function storedLastBkSetUpdateSeqNo() external view returns (uint64);
            /// Prover bundle width the `applyBkSetUpdate` gate divides by.
            function bundleStride() external view returns (uint64);
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

            event LayerAnchorAppended(uint8 indexed layer, uint256 hashValue, uint64 blockHeight);

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
                uint256 anchorLayer;
                uint256 dstBridge;
            }

            function withdrawByProof(
                bytes calldata proof,
                WithdrawalPublicInputs calldata pub
            ) external returns (bool success);

            function isNullifierUsed(uint256 nullifier) external view returns (bool);

            /// Custom errors `AckiNackiBridge.withdrawByProof` can revert with:
            /// its own checks, the `whenNotPaused` and `nonReentrant`
            /// modifiers, and the treasury helpers it calls. Listed here so
            /// the sol! macro emits `SELECTOR` constants — the relayer
            /// withdraw scan uses them to distinguish permanent
            /// (proof-intrinsic) reverts from transient ones (e.g. anchor
            /// not yet registered by the verifyBlock lane) so a permanently
            /// bad proof does not hold up the whole queue — and so
            /// `decode_withdraw_revert` can name them.
            error WithdrawByProofDisabled();
            error WithdrawalProofRejected();
            error NullifierAlreadyUsed(uint256 nullifier);
            error FieldElementOutOfRange(uint256 value);
            error DstChainIdMismatch(uint256 supplied, uint256 expected);
            error WrongDstBridge(uint256 dstBridge);
            error RecipientHalfOutOfRange(uint256 value);
            error WithdrawIdentityMismatch();
            error UnknownAnchor(uint256 finalRoot);
            error InvalidNumLayers(uint256 anchorLayer);
            error UnsupportedTokenId(uint256 tokenId);
            error InvalidRecipient();
            error WithdrawTreasuryShortfall(uint256 requested, uint256 available);
            error BridgePaused();
            error Reentrancy();
            error LayerOutOfRange(uint8 layer);
            error InsufficientTreasury();
            error AaveWithdrawFailed(uint256 requested, uint256 received);
            error WithdrawTransferFailed(address recipient, uint256 amount);

            function nacklState() external view returns (uint256 minted, bool handedOver);
            error NacklMinterGone();
            error WithdrawalWindowFull(uint32 tokenId, uint256 used, uint256 amount, uint256 dailyLimit);
            error NacklNotMinter(address token);

            /// Read-only getters the end-user CLI preflights the deploy
            /// with, before the irreversible Acki Nacki burn. All four are
            /// plain public state / immutables on `AckiNackiBridge`.
            function treasuryBalance() external view returns (uint256);
            function usdcTokenId() external view returns (uint32);
            function withdrawalWindow(uint32 tokenId) external view returns (uint256 dailyLimit, uint256 used);
            function tokenInfo(uint32 tokenId) external view returns (address token, uint8 decimals, bool depositsEnabled, uint256 tvlCap, uint256 dailyLimit, uint256 liabilities);
            function bridgeWithdrawalVerifier() external view returns (address);
            function bridgeWithdrawalDappFr() external view returns (uint256);
            function bridgeWithdrawalAccFr() external view returns (uint256);

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

        /// The two links between `bridgeWithdrawalVerifier` and the
        /// deployed Yul verifier. Declared here so the CLI can walk
        /// `adapter → shplonkVerifier() → yulVerifier()` the same way
        /// `docker/relayer/preflight.sh` does: a non-zero
        /// adapter address proves nothing on its own.
        #[sol(rpc)]
        #[allow(missing_docs)]
        contract ShplonkAdapter {
            function shplonkVerifier() external view returns (address);
            /// Poseidon digest of the inner-circuit VK the adapter's
            /// constructor pinned. Compared to calldata word `12 + N` by
            /// every runtime verify call in `ShplonkAggregatorVerifierBase`;
            /// preflight reads it so a wrong pin is refused before the
            /// (irreversible) AN-side burn instead of surfacing as a
            /// post-burn `WithdrawalProofRejected` revert.
            function vkDigest() external view returns (bytes32);
        }

        #[sol(rpc)]
        #[allow(missing_docs)]
        contract ShplonkWrapper {
            function yulVerifier() external view returns (address);
        }
    }
}

pub use sol_bindings::AckiNackiBridge;

#[derive(Debug, Clone, Copy)]
pub struct TokenPayoutInfo {
    pub token: Address,
    pub decimals: u8,
    pub daily_limit: U256,
    pub liabilities: U256,
}

/// Production bridge client — wraps `sol!`-generated bindings.
///
/// Generic over any alloy [`Provider`] (a wallet-filled provider in
/// production; a plain HTTP provider in read-only smoke tests).
pub struct EthBridgeClient<P: Provider<N>, N: Network = alloy::network::Ethereum> {
    contract: AckiNackiBridge::AckiNackiBridgeInstance<P, N>,
    address: Address,
    /// The account this client sends from ([`Self::with_sender`]). With it
    /// every transaction carries its nonce and fees from [`crate::nonce`];
    /// without it the provider's fillers choose them.
    sender: Option<Address>,
    /// How long [`Self::submit_withdraw`] waits for the receipt
    /// ([`Self::with_withdraw_receipt_timeout`]); `None` waits until it
    /// comes.
    withdraw_receipt_timeout: Option<Duration>,
    /// [`RECEIPT_POLL_INTERVAL`] and [`REPLACED_AFTER`] outside tests.
    receipt_poll_every: Duration,
    replaced_after: Duration,
    /// Held from a transaction's nonce plan to its receipt, so that the
    /// tasks sharing this client (daemon-live's `verifyBlock` and
    /// `applyBkSetUpdate`, and its withdrawal claimer's `withdrawByProof`)
    /// send one at a time: [`crate::nonce`] replaces whatever waits at the
    /// account's next nonce, and two sends planned at once would take the
    /// same one.
    send_lock: Arc<tokio::sync::Mutex<()>>,
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
            sender: None,
            withdraw_receipt_timeout: None,
            receipt_poll_every: RECEIPT_POLL_INTERVAL,
            replaced_after: REPLACED_AFTER,
            send_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Sends from `sender`, the address of the provider's signer, with the
    /// nonce and fees [`crate::nonce`] plans: read from the chain before
    /// every transaction, replacing our own transaction while it waits
    /// unmined.
    pub fn with_sender(mut self, sender: Address) -> Self {
        self.sender = Some(sender);
        self
    }

    /// Stops [`Self::submit_withdraw`] waiting for the receipt after
    /// `timeout`; the payout then comes back unconfirmed, with its
    /// transaction hash in the reason. For the daemons, which retry and skip
    /// a payout that has landed by its nullifier. Without it the receipt is
    /// waited for until it comes, as a one-shot command needs.
    pub fn with_withdraw_receipt_timeout(mut self, timeout: Duration) -> Self {
        self.withdraw_receipt_timeout = Some(timeout);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_receipt_polling(
        mut self,
        every: Duration,
        replaced_after: Duration,
    ) -> Self {
        self.receipt_poll_every = every;
        self.replaced_after = replaced_after;
        self
    }

    /// `call` ready to go out from [`Self::with_sender`]'s account, with the
    /// nonce it takes: its gas estimated first, so a call that would revert
    /// leaves the nonce and fees alone, then the nonce and fees
    /// [`crate::nonce`] plans, remembered before the send. With the gas and
    /// fees set, `send()` estimates nothing more and only broadcasts.
    /// Without a sender `call` comes back as it is, for the provider's
    /// fillers, and no nonce. `Err` is the text of the failed estimate or
    /// plan, the same a failed `send()` reports.
    async fn prepare_send<'a, C: SolCall>(
        &'a self,
        call: alloy::contract::SolCallBuilder<&'a P, C, N>,
    ) -> Result<(alloy::contract::SolCallBuilder<&'a P, C, N>, Option<u64>), String> {
        let Some(sender) = self.sender else {
            return Ok((call, None));
        };
        let call = call.from(sender);
        let gas = call.estimate_gas().await.map_err(|e| e.to_string())?;
        let plan = crate::nonce::plan_transaction(self.contract.provider(), sender)
            .await
            .map_err(|e| e.to_string())?;
        crate::nonce::remember(sender, &plan);
        let call = call
            .gas(gas)
            .nonce(plan.nonce)
            .max_fee_per_gas(plan.max_fee_per_gas)
            .max_priority_fee_per_gas(plan.max_priority_fee_per_gas);
        Ok((call, Some(plan.nonce)))
    }

    /// The receipt of a transaction just sent, for at most `timeout` when
    /// one is set: [`Self::wait_for_receipt`] when [`Self::prepare_send`]
    /// planned its nonce, alloy's receipt watcher otherwise. `Err` names the
    /// transaction.
    async fn confirm(
        &self,
        pending: alloy::providers::PendingTransactionBuilder<N>,
        nonce: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<N::ReceiptResponse, String> {
        let hash = *pending.tx_hash();
        match (self.sender, nonce) {
            (Some(sender), Some(nonce)) => {
                self.wait_for_receipt(sender, nonce, hash, timeout).await
            },
            _ => pending
                .with_timeout(timeout)
                .get_receipt()
                .await
                .map_err(|e| format!("{e} (transaction {hash})")),
        }
    }

    /// Polls for the receipt of `hash`, sent from `sender` at `nonce`.
    /// Every transaction takes the account's next nonce and replaces
    /// whatever waits there ([`crate::nonce`]), so another process sending
    /// from the same key, or the other leg of `daemon-bridge`, can replace
    /// this one. Once the account's `latest` count has been past `nonce`
    /// for [`REPLACED_AFTER`] with no receipt for `hash`, another
    /// transaction took the nonce and this one can never be mined: the wait
    /// ends there instead of running into `timeout` or, without one, going
    /// on forever. The grace covers an RPC whose receipts lag its counts.
    async fn wait_for_receipt(
        &self,
        sender: Address,
        nonce: u64,
        hash: B256,
        timeout: Option<Duration>,
    ) -> Result<N::ReceiptResponse, String> {
        let provider = self.contract.provider();
        let started = tokio::time::Instant::now();
        let mut nonce_taken_since: Option<tokio::time::Instant> = None;
        loop {
            match provider.get_transaction_receipt(hash).await {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => {},
                Err(e) => tracing::debug!(%hash, error = %e, "receipt poll failed; polling on"),
            }
            match provider.get_transaction_count(sender).latest().await {
                Ok(latest) if latest > nonce => {
                    let since = *nonce_taken_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= self.replaced_after {
                        return Err(format!(
                            "transaction {hash} was replaced: another transaction from {sender} \
                             took its nonce {nonce}; send it again"
                        ));
                    }
                },
                Ok(_) => nonce_taken_since = None,
                Err(e) => tracing::debug!(%sender, error = %e, "nonce poll failed; polling on"),
            }
            if let Some(timeout) = timeout {
                if started.elapsed() >= timeout {
                    return Err(format!(
                        "no receipt for transaction {hash} within {} s",
                        timeout.as_secs()
                    ));
                }
            }
            tokio::time::sleep(self.receipt_poll_every).await;
        }
    }

    /// One `storedLastSeenBlockSeqNo` view call at `latest`: the cheap probe
    /// for "has the covering bundle landed yet" polls.
    pub async fn stored_last_seen_block_seq_no(&self) -> Result<u64, RelayerError> {
        self.contract
            .storedLastSeenBlockSeqNo()
            .call()
            .await
            .map_err(map_contract_err)
    }

    /// The hash of block `number`, from `eth_getBlockByNumber` as a raw
    /// request so that only the `hash` field matters. A missing block is a
    /// lagging backend, which a re-read settles.
    async fn block_hash_at(&self, number: u64) -> Result<B256, RelayerError> {
        let block: serde_json::Value = self
            .contract
            .provider()
            .raw_request(
                "eth_getBlockByNumber".into(),
                (format!("0x{number:x}"), false),
            )
            .await
            .map_err(|e| RelayerError::other(format!("eth_getBlockByNumber({number}): {e}")))?;
        block
            .get("hash")
            .and_then(|h| h.as_str())
            .and_then(|h| h.parse::<B256>().ok())
            .ok_or_else(|| {
                RelayerError::other(format!(
                    "eth_getBlockByNumber({number}): no such block yet (a lagging backend?)"
                ))
            })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    async fn fetch_prev_bk_set(&self) -> Result<U256, RelayerError> {
        self.contract
            .storedPrevBkSetCommitment()
            .call()
            .await
            .map_err(map_contract_err)
    }

    /// The contract's immutable `bundleStride`. Fails on a bridge deployed
    /// before the field existed.
    pub async fn bundle_stride(&self) -> Result<u64, RelayerError> {
        self.contract
            .bundleStride()
            .call()
            .await
            .map_err(map_contract_err)
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
        let send_failed = |e: &dyn std::fmt::Display| {
            let reason = format!("withdrawByProof send failed: {e}");
            let permanent = classify_withdraw_revert(&reason) == WithdrawRevertKind::Permanent;
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent,
            }
        };
        let _sending = self.send_lock.lock().await;
        let (call, nonce) = match self.prepare_send(call).await {
            Ok(prepared) => prepared,
            Err(e) => return Ok(send_failed(&e)),
        };
        match call.send().await {
            Ok(pending) => match self
                .confirm(pending, nonce, self.withdraw_receipt_timeout)
                .await
            {
                Ok(receipt) => Ok(outcome_from_mined_withdraw(
                    receipt.status(),
                    receipt.transaction_hash(),
                )),
                Err(e) => {
                    // Post-send confirmation failures (RPC dropped, receipt
                    // wait timed out, transaction replaced) are transient by
                    // nature — the tx may still land, or a new send pays out —
                    // so we do not park the proof here.
                    Ok(WithdrawSubmitOutcome::Reverted {
                        reason: format!("tx confirmation error: {e}"),
                        permanent: false,
                    })
                },
            },
            Err(e) => Ok(send_failed(&e)),
        }
    }

    pub async fn nackl_state(&self) -> Result<NacklState, RelayerError> {
        let r = self
            .contract
            .nacklState()
            .call()
            .await
            .map_err(|e| RelayerError::other(format!("nacklState: {e}")))?;
        Ok(NacklState {
            minted: r.minted,
            handed_over: r.handedOver,
        })
    }

    /// Submit `applyBkSetUpdate` for a BK-set rotation bundle (`bkupd_*.json`).
    pub async fn submit_bk_set_update(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        self.send_bk_set_update_checked(update).await
    }

    /// [`Self::send_bk_set_update`] with the guards `submit_block` already
    /// has: a bound on the wait for the receipt ([`BK_UPDATE_RECEIPT_TIMEOUT`];
    /// alloy waits forever by default, which would hang the daemon on a
    /// dropped transaction) and a post-submit check that the slots the
    /// rotation writes now hold what was submitted. The receipt's status is
    /// not checked anywhere else, so a transaction mined but reverted would
    /// otherwise come back as `Applied`.
    async fn send_bk_set_update_checked(
        &self,
        update: &BkSetUpdateData,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        let outcome =
            match tokio::time::timeout(BK_UPDATE_RECEIPT_TIMEOUT, self.send_bk_set_update(update))
                .await
            {
                Ok(outcome) => outcome?,
                Err(_) => {
                    return self
                        .bk_update_after_receipt_timeout(
                            update.block_seq_no,
                            update.new_commitment_l3,
                        )
                        .await;
                },
            };
        if let BkSetUpdateSubmitOutcome::Applied {
            new_state, ..
        } = &outcome
        {
            if new_state.last_bk_set_update_seq_no != update.block_seq_no
                || new_state.bk_set_commitment != update.new_commitment_l3
            {
                return Ok(BkSetUpdateSubmitOutcome::Reverted {
                    reason: format!(
                        "post-submit drift: chain last_bk_set_update_seq_no={} \
                         bk_set_commitment={:#x} != submitted N={} new commitment={:#x}",
                        new_state.last_bk_set_update_seq_no,
                        new_state.bk_set_commitment,
                        update.block_seq_no,
                        update.new_commitment_l3
                    ),
                });
            }
        }
        Ok(outcome)
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
            update.attestation_last_seen,
            update.old_commitment_l2,
            update.new_commitment_l3,
            B256::from(update.sibling_h01),
            B256::from(update.sibling_h4_7),
            B256::from(update.sibling_h8_15),
        );
        let _sending = self.send_lock.lock().await;
        let (call, nonce) = match self.prepare_send(call).await {
            Ok(prepared) => prepared,
            Err(e) => {
                return Ok(BkSetUpdateSubmitOutcome::Reverted {
                    reason: format!("applyBkSetUpdate send failed: {e}"),
                });
            },
        };
        // No bound here: `send_bk_set_update_checked` bounds the whole call.
        match call.send().await {
            Ok(pending) => match self.confirm(pending, nonce, None).await {
                Ok(receipt) => {
                    let bn = receipt.block_number().ok_or_else(|| {
                        RelayerError::other("applyBkSetUpdate receipt missing block_number")
                    })?;
                    // Same pin as `submit_block`: `latest` on a
                    // load-balanced RPC can tear the five-slot snapshot
                    // and poison Check B.
                    let new_state = self.read_state_at(BlockId::from(bn)).await?;
                    Ok(BkSetUpdateSubmitOutcome::Applied {
                        new_state,
                        tx_hash: Some(receipt.transaction_hash()),
                    })
                },
                Err(e) => Ok(BkSetUpdateSubmitOutcome::Reverted {
                    reason: format!("tx confirmation error: {e}"),
                }),
            },
            Err(e) => Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!("applyBkSetUpdate send failed: {e}"),
            }),
        }
    }

    /// The receipt wait ended without a receipt. If this rotation is already
    /// the contract's current BK set, the transaction landed and the receipt
    /// was only late. A read that fails, or a contract that has not moved to
    /// this seq_no and commitment, stays an unconfirmed failure.
    async fn bk_update_after_receipt_timeout(
        &self,
        seq_no: u64,
        new_commitment: U256,
    ) -> Result<BkSetUpdateSubmitOutcome, RelayerError> {
        if let Ok(state) = self.read_state_at(BlockId::latest()).await {
            if state.last_bk_set_update_seq_no == seq_no
                && state.bk_set_commitment == new_commitment
            {
                return Ok(BkSetUpdateSubmitOutcome::Applied {
                    new_state: state,
                    tx_hash: None,
                });
            }
        }
        Ok(BkSetUpdateSubmitOutcome::Reverted {
            reason: format!(
                "tx confirmation error: no receipt within {} s",
                BK_UPDATE_RECEIPT_TIMEOUT.as_secs()
            ),
        })
    }

    /// Same recovery as [`Self::bk_update_after_receipt_timeout`] for
    /// `verifyBlock`: a cursor already at the submitted seq_no means the
    /// transaction landed.
    async fn verify_block_after_receipt_timeout(
        &self,
        block_seq_no: u64,
        confirm_error: String,
    ) -> Result<SubmitOutcome, RelayerError> {
        if let Ok(state) = self.read_state_at(BlockId::latest()).await {
            if state.last_seen_block_seq_no == block_seq_no {
                return Ok(SubmitOutcome::Verified {
                    new_state: state,
                    tx_hash: None,
                });
            }
        }
        Ok(SubmitOutcome::Reverted {
            reason: format!("tx confirmation error: {confirm_error}"),
        })
    }

    pub async fn is_nullifier_used(&self, nullifier: U256) -> Result<bool, RelayerError> {
        self.contract
            .isNullifierUsed(nullifier)
            .call()
            .await
            .map_err(map_contract_err)
    }

    /// `uint256 public treasuryBalance` (`AckiNackiBridge.sol:98`).
    pub async fn treasury_balance(&self) -> Result<U256, RelayerError> {
        self.contract
            .treasuryBalance()
            .call()
            .await
            .map_err(map_contract_err)
    }

    /// The rolling 24-hour withdrawal limit of `token_id` and how much of it
    /// is used: `withdrawByProof` reverts `WithdrawalWindowFull` for a payout
    /// above `dailyLimit - used`.
    pub async fn withdrawal_window(&self, token_id: u32) -> Result<(U256, U256), RelayerError> {
        let r = self
            .contract
            .withdrawalWindow(token_id)
            .call()
            .await
            .map_err(map_contract_err)?;
        Ok((r.dailyLimit, r.used))
    }

    /// Refuse a USDC payout lane that does not match the AN bridge's ECC id,
    /// then read its withdrawal window.
    pub async fn usdc_withdrawal_window(&self) -> Result<(U256, U256), RelayerError> {
        let token_id = self
            .contract
            .usdcTokenId()
            .call()
            .await
            .map_err(map_contract_err)?;
        if token_id != 3 {
            return Err(RelayerError::other(format!(
                "USDC token id {token_id} does not match AN ECC id 3"
            )));
        }
        self.withdrawal_window(token_id).await
    }

    pub async fn token_payout_info(&self, token_id: u32) -> Result<TokenPayoutInfo, RelayerError> {
        let info = self
            .contract
            .tokenInfo(token_id)
            .call()
            .await
            .map_err(map_contract_err)?;
        if info.token == Address::ZERO {
            return Err(RelayerError::other(format!(
                "token id {token_id} is not registered"
            )));
        }
        Ok(TokenPayoutInfo {
            token: info.token,
            decimals: info.decimals,
            daily_limit: info.dailyLimit,
            liabilities: info.liabilities,
        })
    }

    /// `IBridgeWithdrawalVerifier public immutable bridgeWithdrawalVerifier`
    /// (`AckiNackiBridge.sol:208`). Zero disables withdrawals entirely —
    /// `withdrawByProof` reverts `WithdrawByProofDisabled` (`:1160`).
    pub async fn withdrawal_verifier(&self) -> Result<Address, RelayerError> {
        self.contract
            .bridgeWithdrawalVerifier()
            .call()
            .await
            .map_err(map_contract_err)
    }

    /// The `(dappFr, accFr)` pair the deploy was pinned to
    /// (`AckiNackiBridge.sol:216-219`). A proof whose public inputs [6] and
    /// [7] differ reverts `WithdrawIdentityMismatch` before verification
    /// (`:1164`).
    pub async fn withdrawal_identity(&self) -> Result<(U256, U256), RelayerError> {
        let dapp = self
            .contract
            .bridgeWithdrawalDappFr()
            .call()
            .await
            .map_err(map_contract_err)?;
        let acc = self
            .contract
            .bridgeWithdrawalAccFr()
            .call()
            .await
            .map_err(map_contract_err)?;
        Ok((dapp, acc))
    }

    /// Walk `adapter → shplonkVerifier() → yulVerifier()`, returning both
    /// links. The provider is reused, so this costs two `eth_call`s.
    pub async fn shplonk_stack(
        &self,
        adapter: Address,
    ) -> Result<(Address, Address), RelayerError> {
        let a = sol_bindings::ShplonkAdapter::new(adapter, self.contract.provider());
        let wrapper = a.shplonkVerifier().call().await.map_err(map_contract_err)?;
        let w = sol_bindings::ShplonkWrapper::new(wrapper, self.contract.provider());
        let yul = w.yulVerifier().call().await.map_err(map_contract_err)?;
        Ok((wrapper, yul))
    }

    /// Read the adapter's `vkDigest()` immutable — the Poseidon digest of the
    /// inner-circuit VK its constructor was pinned to. The runtime verify
    /// path in `ShplonkAggregatorVerifierBase` compares this to calldata
    /// word `12 + N`, so preflight can refuse a wrong pin before the
    /// (irreversible) AN-side burn instead of surfacing as a post-burn
    /// `WithdrawalProofRejected` revert.
    pub async fn adapter_vk_digest(&self, adapter: Address) -> Result<B256, RelayerError> {
        let a = sol_bindings::ShplonkAdapter::new(adapter, self.contract.provider());
        a.vkDigest().call().await.map_err(map_contract_err)
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
        let prev_bk = self
            .contract
            .storedPrevBkSetCommitment()
            .block(at)
            .call()
            .await
            .map_err(map_contract_err)?;
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: last,
            bk_set_commitment: bk,
            prev_bk_set_commitment: prev_bk,
            prev_max_level_layer_hash: anchor,
            last_bk_set_update_seq_no: last_bk,
        })
    }

    /// Read every field the daemon needs to reconstruct `BridgeState`
    /// from an already-advanced contract (Case 6 chain-resurrect).
    ///
    /// Issues 5 scalar view calls + 10 `getLayerWindow` calls, all pinned
    /// to one block, whatever the bridge's age. Each window returns ~5 KB,
    /// so the view calls cost ~50 KB of RPC response. The contract stores
    /// no per-slot heights, so `heights` stays zero: an anchor is found by
    /// its hash, the way `withdrawByProof` checks it.
    ///
    /// Consistency: `eth_blockNumber` is taken first, every read is pinned
    /// to that number, and the block's hash is compared before and after,
    /// so a head block replaced mid-read is re-read instead of mixing two
    /// forks, when the RPC observes the replacement (backends on different
    /// forks behind one balancer can still answer the two hash calls
    /// alike). A failed snapshot gets [`READ_FULL_STATE_ATTEMPTS`] reads,
    /// six seconds apart.
    ///
    /// Callers: `daemon-live` once at startup, its withdrawal claimer and
    /// `daemon-withdraw-live` before a proof, the withdrawal CLI after each
    /// coverage poll (one scalar call per round) that observed the target.
    pub async fn read_full_state(&self) -> Result<EthBridgeContractState, RelayerError> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.read_full_state_once().await {
                Ok(state) => return Ok(state),
                Err(err) if attempt < READ_FULL_STATE_ATTEMPTS => {
                    tracing::warn!(
                        attempt,
                        error = %err,
                        "read_full_state failed; retrying the pinned snapshot"
                    );
                    tokio::time::sleep(Duration::from_millis(READ_FULL_STATE_RETRY_DELAY_MS)).await;
                },
                Err(err) => return Err(err),
            }
        }
    }

    async fn read_full_state_once(&self) -> Result<EthBridgeContractState, RelayerError> {
        let head = self
            .contract
            .provider()
            .get_block_number()
            .await
            .map_err(|e| RelayerError::other(format!("get_block_number: {e}")))?;
        // Every read below addresses the head by number, so a head block
        // replaced mid-read would mix two forks (scalars from one, windows
        // from the other) without any call failing. Its hash is taken now
        // and compared after the windows.
        let head_hash = self.block_hash_at(head).await?;
        let at = BlockId::from(head);
        let scalars = self.read_state_at(at).await?;

        // Read all 10 layer windows at `head`.
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
                .block(at)
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
            // On-chain `heights` are not written.
            let heights = vec![0u64; w.heights.len()];
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
        let head_hash_after = self.block_hash_at(head).await?;
        if head_hash_after != head_hash {
            return Err(RelayerError::other(format!(
                "block {head} was replaced during the read ({head_hash} -> {head_hash_after})"
            )));
        }
        let layer_windows: [HistoryWindow; MAX_LAYER_HASHES] =
            windows.try_into().map_err(|_| {
                RelayerError::Other("read_full_state: expected 10 layer windows".into())
            })?;

        Ok(EthBridgeContractState {
            last_seen_block_seq_no: scalars.last_seen_block_seq_no,
            bk_set_commitment: scalars.bk_set_commitment.to_le_bytes::<32>(),
            last_bk_set_update_seq_no: scalars.last_bk_set_update_seq_no,
            prev_bk_set_commitment: scalars.prev_bk_set_commitment.to_le_bytes::<32>(),
            genesis_prev_max_level_layer_hash: scalars
                .prev_max_level_layer_hash
                .to_le_bytes::<32>(),
            layer_windows,
        })
    }
}

/// Outcome of [`EthBridgeClient::submit_withdraw`].
#[derive(Clone, Debug)]
pub enum WithdrawSubmitOutcome {
    Paid {
        tx_hash: B256,
    },
    /// `withdrawByProof` reverted. `permanent = true` means the revert is
    /// intrinsic to the proof (e.g. `WithdrawalProofRejected` — includes
    /// the wrong-VK-digest path where the aggregator's inner-VK guard
    /// makes the adapter return `false`; `WithdrawIdentityMismatch`;
    /// `DstChainIdMismatch`) so the withdraw scan parks the
    /// `proof_event_*.json` instead of holding the queue up on
    /// exponential backoff. `permanent = false` covers transient reverts
    /// like `UnknownAnchor` (verifyBlock lane hasn't landed the anchor
    /// yet) and `WithdrawTreasuryShortfall`, plus every alloy-side
    /// non-revert error (RPC, gas, nonce) — those still retry with
    /// backoff.
    Reverted {
        reason: String,
        permanent: bool,
    },
}

pub const NACKL_TOKEN_ID: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NacklState {
    pub minted: U256,
    pub handed_over: bool,
}

/// The outcome of a `withdrawByProof` that was mined, from its receipt.
///
/// A receipt proves inclusion, not success: `status == false` is a revert
/// that happened on chain, after the gas estimate passed — state moved
/// between the estimate and inclusion (the anchor was evicted, the treasury
/// drained, the nullifier spent by another relayer). The receipt carries no
/// revert reason, so the outcome is `Reverted` with `permanent = false`: the
/// proof is retried with backoff rather than parked, and if the cause is
/// intrinsic the next send fails at the estimate with a decoded reason that
/// `classify_withdraw_revert` can park on.
///
/// Mapping every receipt to `Paid` was wrong in exactly one way that matters:
/// the burn on Acki Nacki is already irreversible when this runs, so a revert
/// here is a user who is not paid — and `withdraw_scan_once` would have put
/// the proof in `done`, logged `PAID` and raised `st.paid`, with nothing left
/// to prompt a retry.
pub fn outcome_from_mined_withdraw(status: bool, tx_hash: B256) -> WithdrawSubmitOutcome {
    if status {
        WithdrawSubmitOutcome::Paid {
            tx_hash,
        }
    } else {
        WithdrawSubmitOutcome::Reverted {
            reason: format!(
                "withdrawByProof reverted on chain (tx {tx_hash:#x}); the receipt carries no \
                 reason, the next send will report it"
            ),
            permanent: false,
        }
    }
}

/// Classify a `withdrawByProof` revert reason as permanent (proof-intrinsic;
/// park the `proof_event_*.json`) or transient (retry with backoff).
///
/// The `reason` is the stringified [`alloy::contract::Error`] chain the
/// production wrapper produces via `format!("{e}")`. Alloy renders unknown
/// custom-error reverts as `... 0x<selector><args>`, so a substring match
/// on the 4-byte selector hex is enough to identify each Solidity error we
/// declared in the sol! block above. Errors that could go either way
/// (e.g. anything the RPC returned as a plain string) default to
/// transient — the daemon's existing backoff was the pre-classification
/// behaviour and remains a safe fallback.
pub fn classify_withdraw_revert(reason: &str) -> WithdrawRevertKind {
    use alloy::sol_types::SolError;
    let permanent_selectors: [[u8; 4]; 11] = [
        AckiNackiBridge::WithdrawalProofRejected::SELECTOR,
        AckiNackiBridge::WithdrawIdentityMismatch::SELECTOR,
        AckiNackiBridge::DstChainIdMismatch::SELECTOR,
        AckiNackiBridge::WrongDstBridge::SELECTOR,
        AckiNackiBridge::RecipientHalfOutOfRange::SELECTOR,
        AckiNackiBridge::InvalidRecipient::SELECTOR,
        AckiNackiBridge::FieldElementOutOfRange::SELECTOR,
        AckiNackiBridge::InvalidNumLayers::SELECTOR,
        AckiNackiBridge::NullifierAlreadyUsed::SELECTOR,
        AckiNackiBridge::NacklMinterGone::SELECTOR,
        AckiNackiBridge::LayerOutOfRange::SELECTOR,
    ];
    let lowered = reason.to_ascii_lowercase();
    for sel in permanent_selectors {
        // Alloy renders selectors with an `0x` prefix; the substring form
        // works whether the error chain is `revert data: 0x…`, an
        // `execution reverted: 0x…` wrap, or a decoded `<Name>()` form
        // (name lookup would still contain the hex on unknown-ABI paths).
        let hex = format!("0x{}", hex::encode(sel));
        if lowered.contains(&hex) {
            return WithdrawRevertKind::Permanent;
        }
    }
    WithdrawRevertKind::Transient
}

/// A `withdrawByProof` revert decoded into the bridge's custom error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeRevert {
    /// The Solidity error's name, e.g. `NullifierAlreadyUsed`.
    pub name: &'static str,
    /// `Name(argument: value, …)`: field elements and hashes in 32-byte hex,
    /// amounts and chain ids in decimal.
    pub rendered: String,
}

/// Find the revert data in `reason` — the text
/// [`EthBridgeClient::dry_run_withdraw`] and
/// [`EthBridgeClient::submit_withdraw`] report, the same text
/// [`classify_withdraw_revert`] reads — and decode it as one of
/// `AckiNackiBridge`'s custom errors.
///
/// Nodes put the data in different places (`data: "0x…"`, or inline after
/// `execution reverted:`), so every `0x`-prefixed run of at least four
/// bytes is tried and the first that decodes wins. `None` when nothing
/// decodes: a transport failure, or a revert raised by something other than
/// the bridge.
pub fn decode_withdraw_revert(reason: &str) -> Option<BridgeRevert> {
    use alloy::sol_types::SolInterface;
    hex_runs(reason).find_map(|bytes| {
        AckiNackiBridge::AckiNackiBridgeErrors::abi_decode(&bytes)
            .ok()
            .map(render_bridge_error)
    })
}

/// Every `0x`-prefixed run of hex digits in `text` that is a whole number of
/// bytes and at least a selector long, decoded.
fn hex_runs(text: &str) -> impl Iterator<Item = Vec<u8>> + '_ {
    text.match_indices("0x").filter_map(move |(i, _)| {
        let digits: String = text[i + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if digits.len() < 8 || !digits.len().is_multiple_of(2) {
            return None;
        }
        hex::decode(&digits).ok()
    })
}

/// Name a decoded error and its arguments. Exhaustive on purpose: an error
/// added to the `sol!` block above does not compile until it is named here.
fn render_bridge_error(e: AckiNackiBridge::AckiNackiBridgeErrors) -> BridgeRevert {
    use AckiNackiBridge::AckiNackiBridgeErrors as E;
    let word = |v: U256| format!("0x{v:064x}");
    let (name, args) = match e {
        E::WithdrawByProofDisabled(_) => ("WithdrawByProofDisabled", String::new()),
        E::WithdrawalProofRejected(_) => ("WithdrawalProofRejected", String::new()),
        E::NullifierAlreadyUsed(x) => (
            "NullifierAlreadyUsed",
            format!("nullifier: {}", word(x.nullifier)),
        ),
        E::FieldElementOutOfRange(x) => (
            "FieldElementOutOfRange",
            format!("value: {}", word(x.value)),
        ),
        E::DstChainIdMismatch(x) => (
            "DstChainIdMismatch",
            format!("supplied: {}, expected: {}", x.supplied, x.expected),
        ),
        E::RecipientHalfOutOfRange(x) => (
            "RecipientHalfOutOfRange",
            format!("value: {}", word(x.value)),
        ),
        E::WithdrawIdentityMismatch(_) => ("WithdrawIdentityMismatch", String::new()),
        E::UnknownAnchor(x) => ("UnknownAnchor", format!("finalRoot: {}", word(x.finalRoot))),
        E::WrongDstBridge(x) => (
            "WrongDstBridge",
            format!("dstBridge: 0x{:040x}", x.dstBridge),
        ),
        E::InvalidNumLayers(x) => (
            "InvalidNumLayers",
            format!("anchorLayer: {}", x.anchorLayer),
        ),
        E::UnsupportedTokenId(x) => ("UnsupportedTokenId", format!("tokenId: {}", x.tokenId)),
        E::InvalidRecipient(_) => ("InvalidRecipient", String::new()),
        E::WithdrawTreasuryShortfall(x) => (
            "WithdrawTreasuryShortfall",
            format!("requested: {}, available: {}", x.requested, x.available),
        ),
        E::BridgePaused(_) => ("BridgePaused", String::new()),
        E::Reentrancy(_) => ("Reentrancy", String::new()),
        E::LayerOutOfRange(x) => ("LayerOutOfRange", format!("layer: {}", x.layer)),
        E::InsufficientTreasury(_) => ("InsufficientTreasury", String::new()),
        E::AaveWithdrawFailed(x) => (
            "AaveWithdrawFailed",
            format!("requested: {}, received: {}", x.requested, x.received),
        ),
        E::WithdrawTransferFailed(x) => (
            "WithdrawTransferFailed",
            format!("recipient: {}, amount: {}", x.recipient, x.amount),
        ),
        E::NacklMinterGone(_) => ("NacklMinterGone", String::new()),
        E::WithdrawalWindowFull(x) => (
            "WithdrawalWindowFull",
            format!(
                "tokenId: {}, used: {}, amount: {}, dailyLimit: {}",
                x.tokenId, x.used, x.amount, x.dailyLimit
            ),
        ),
        E::NacklNotMinter(x) => ("NacklNotMinter", format!("token: {}", x.token)),
    };
    BridgeRevert {
        name,
        rendered: format!("{name}({args})"),
    }
}

/// See [`classify_withdraw_revert`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WithdrawRevertKind {
    Permanent,
    Transient,
}

/// Outcome of [`EthBridgeClient::submit_bk_set_update`].
#[derive(Clone, Debug)]
pub enum BkSetUpdateSubmitOutcome {
    Applied {
        new_state: BridgeOnChainState,
        tx_hash: Option<B256>,
    },
    Reverted {
        reason: String,
    },
}

/// Bound on the wait for an `applyBkSetUpdate` receipt — the same 180 s
/// (~15 Sepolia blocks) `submit_block` allows `verifyBlock`.
pub const BK_UPDATE_RECEIPT_TIMEOUT: Duration = Duration::from_secs(180);

/// Bound on the wait for a `withdrawByProof` receipt in `daemon-withdraw` and
/// `daemon-bridge` ([`EthBridgeClient::with_withdraw_receipt_timeout`]), the
/// same 180 s. alloy waits forever by default, which held the withdraw scan
/// on a dropped transaction; past the bound the proof is retried like any
/// other unconfirmed send, and its nullifier check skips it once it has
/// landed. A one-shot `withdrawByProof` — `ackinacki-bridge withdraw` among
/// them — waits for its receipt.
pub const WITHDRAW_RECEIPT_TIMEOUT: Duration = Duration::from_secs(180);

/// How often a wait for a receipt polls the receipt and the sender's
/// `latest` count: a third of a Sepolia block.
pub const RECEIPT_POLL_INTERVAL: Duration = Duration::from_secs(4);

/// How long the sender's `latest` count must stay past a transaction's
/// nonce, with no receipt for it, before the wait takes it for replaced.
pub const REPLACED_AFTER: Duration = Duration::from_secs(30);

/// `true` when a `verifyBlock` or `applyBkSetUpdate` failure says nothing
/// about the transaction itself: the RPC refused or dropped the request
/// (rate limit, connection, nonce, fee) or the receipt never came. The
/// transaction may still have landed; the next tick re-reads the chain and
/// finds out. A revert the node simulated before sending (`execution
/// reverted`, JSON-RPC error 3) and a failed post-submit check are not
/// unconfirmed: they say the contract refused, and count toward the abort
/// threshold.
pub fn is_unconfirmed_submit_failure(reason: &str) -> bool {
    if reason.starts_with("tx confirmation error:") {
        return true;
    }
    let send_failed = reason.starts_with("verifyBlock send failed:")
        || reason.starts_with("applyBkSetUpdate send failed:");
    if !send_failed {
        return false;
    }
    let lowered = reason.to_ascii_lowercase();
    !(lowered.contains("revert") || lowered.contains("error code 3:"))
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
        anchorLayer: pub_inputs.anchor_layer,
        dstBridge: pub_inputs.dst_bridge,
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
        let prev_bk = self.fetch_prev_bk_set().await?;
        Ok(BridgeOnChainState {
            last_seen_block_seq_no: last,
            bk_set_commitment: bk,
            prev_bk_set_commitment: prev_bk,
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
                    Err(e) => {
                        tracing::warn!("dump-submissions: write {} failed: {e}", path.display())
                    },
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

        let _sending = self.send_lock.lock().await;
        let (call, nonce) = match self.prepare_send(call).await {
            Ok(prepared) => prepared,
            Err(e) => {
                return Ok(SubmitOutcome::Reverted {
                    reason: format!("verifyBlock send failed: {e}"),
                });
            },
        };
        let send_res = call.send().await;
        let (tx_hash, receipt_block) = match send_res {
            Ok(pending) => match self.confirm(pending, nonce, Some(RECEIPT_TIMEOUT)).await {
                Ok(receipt) => {
                    let bn = receipt.block_number().ok_or_else(|| {
                        RelayerError::other("verifyBlock receipt missing block_number")
                    })?;
                    (Some(receipt.transaction_hash()), bn)
                },
                Err(e) => {
                    return self
                        .verify_block_after_receipt_timeout(block.block_seq_no, e.to_string())
                        .await;
                },
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
                    "post-submit drift: expectedPrevAnchor({})={} != top layer of submitted \
                     block={}",
                    block.num_layers, post_anchor, expected_top
                ),
            });
        }
        let expected_bk = new_state.expected_bk_set_for(block.block_seq_no);
        if expected_bk != block.bk_set_commitment {
            return Ok(SubmitOutcome::Reverted {
                reason: format!(
                    "post-submit drift: expected_bk_set_for({})={} != submitted={}",
                    block.block_seq_no, expected_bk, block.bk_set_commitment
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
        self.send_bk_set_update_checked(update).await
    }

    async fn expected_prev_anchor(&self, num_layers: u8) -> Result<U256, RelayerError> {
        self.contract
            .expectedPrevAnchor(num_layers)
            .call()
            .await
            .map_err(map_contract_err)
    }

    async fn read_state_pinned(&self) -> Result<BridgeOnChainState, RelayerError> {
        let head = self
            .contract
            .provider()
            .get_block_number()
            .await
            .map_err(|e| RelayerError::other(format!("get_block_number: {e}")))?;
        self.read_state_at(BlockId::from(head)).await
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

    /// A mined `withdrawByProof` with status 0 must not read as paid: the
    /// user's eccUSDC are already burned, so `Paid` would park an unpaid
    /// withdrawal in `done` behind a `PAID` log line.
    #[test]
    fn a_mined_revert_is_reverted_not_paid() {
        let hash = B256::repeat_byte(0x42);
        match outcome_from_mined_withdraw(false, hash) {
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent,
            } => {
                assert!(
                    !permanent,
                    "a receipt carries no reason, so it cannot be parked on"
                );
                assert!(reason.contains("reverted on chain"), "{reason}");
                assert!(reason.contains(&format!("{hash:#x}")), "{reason}");
            },
            WithdrawSubmitOutcome::Paid {
                ..
            } => panic!("status 0 reported as Paid"),
        }
        assert!(matches!(
            outcome_from_mined_withdraw(true, hash),
            WithdrawSubmitOutcome::Paid { tx_hash } if tx_hash == hash
        ));
    }

    /// `ackinacki-bridge withdraw` builds its client with `new`: a payout
    /// slower than the daemons' bound must not come back to the user as a
    /// failure. Only the daemons set a bound.
    #[test]
    fn only_the_daemons_stop_waiting_for_a_withdraw_receipt() {
        let provider = alloy::providers::RootProvider::<alloy::network::Ethereum>::new_http(
            "http://127.0.0.1:1".parse().unwrap(),
        );
        let client = EthBridgeClient::new(Address::ZERO, provider);
        assert_eq!(client.withdraw_receipt_timeout, None);
        let client = client.with_withdraw_receipt_timeout(WITHDRAW_RECEIPT_TIMEOUT);
        assert_eq!(
            client.withdraw_receipt_timeout,
            Some(WITHDRAW_RECEIPT_TIMEOUT)
        );
    }

    #[test]
    fn unconfirmed_submit_failures_are_rpc_and_receipt_failures_only() {
        // The RPC refused or dropped the request: nothing is known about the
        // transaction, so it must not count toward the abort threshold.
        for reason in [
            "verifyBlock send failed: server returned an error response: error code -32005: rate \
             limit exceeded",
            "applyBkSetUpdate send failed: HTTP error 429 with body: Too Many Requests",
            "applyBkSetUpdate send failed: error sending request for url (https://rpc): \
             connection closed",
            "verifyBlock send failed: server returned an error response: error code -32000: nonce \
             too low",
            "tx confirmation error: no receipt within 180 s",
            "tx confirmation error: Timeout",
        ] {
            assert!(is_unconfirmed_submit_failure(reason), "{reason}");
        }
        // The contract refused: a simulated revert or a post-submit mismatch.
        for reason in [
            "verifyBlock send failed: server returned an error response: error code 3: execution \
             reverted, data: \"0x87bf1c06\"",
            "applyBkSetUpdate send failed: server returned an error response: error code -32000: \
             execution reverted",
            "post-submit drift: chain last_seen_block_seq_no=1 != submitted=2",
            "BkSetCommitmentMismatch(supplied=0x1, stored=0x2)",
            "off-chain old_commitment 0x1 != on-chain 0x2",
        ] {
            assert!(!is_unconfirmed_submit_failure(reason), "{reason}");
        }
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
        let outcome = bridge.submit_block(&block(1, anchor)).await.unwrap();
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

    // ─────────────────────────────────────────────────────────────────────
    // Withdraw revert classification (see `classify_withdraw_revert`).
    //
    // The reason strings below imitate the shape alloy's `contract::Error`
    // Display produces for an unknown-ABI custom-error revert — a suffix
    // like `... 0x<selector><args>`. The classifier substring-matches on
    // the selector hex so it doesn't rely on the alloy prose staying
    // stable across versions.
    // ─────────────────────────────────────────────────────────────────────

    fn selector_hex_of<E: alloy::sol_types::SolError>() -> String {
        format!("0x{}", hex::encode(E::SELECTOR))
    }

    #[test]
    fn classify_withdrawal_proof_rejected_is_permanent() {
        // The wrong-inner-VK path: adapter's runtime digest guard makes
        // `verifyWithdrawal` return `false`, so the bridge reverts
        // `WithdrawalProofRejected()`. Must be classified permanent so
        // the withdraw scan parks the proof.
        let reason = format!(
            "withdrawByProof send failed: server returned an error response: error code 3: \
             execution reverted, data: \"{}\"",
            selector_hex_of::<AckiNackiBridge::WithdrawalProofRejected>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Permanent,
        );
    }

    #[test]
    fn classify_withdraw_identity_mismatch_is_permanent() {
        let reason = format!(
            "revert data: {}",
            selector_hex_of::<AckiNackiBridge::WithdrawIdentityMismatch>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Permanent,
        );
    }

    #[test]
    fn proof_for_another_bridge_is_parked_and_names_it() {
        // A proof bound to a different EVM bridge never pays here, whatever
        // the anchor or treasury do later.
        let reason = format!(
            "execution reverted, data: \
             {}0000000000000000000000003fb082a905a895151224f6d93bd074e816338638",
            selector_hex_of::<AckiNackiBridge::WrongDstBridge>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Permanent
        );
        assert_eq!(
            decode_withdraw_revert(&reason).unwrap().rendered,
            "WrongDstBridge(dstBridge: 0x3fb082a905a895151224f6d93bd074e816338638)"
        );
    }

    #[test]
    fn a_token_listing_change_retries_the_existing_proof() {
        let reason = format!(
            "execution reverted: {}",
            selector_hex_of::<AckiNackiBridge::UnsupportedTokenId>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Transient
        );
    }

    #[test]
    fn classify_unknown_anchor_is_transient() {
        // `UnknownAnchor` means the verifyBlock lane hasn't registered
        // the proof's `finalRoot` yet — a retry after the anchor lands
        // will succeed, so the daemon must keep this on the backoff
        // path (not park the file).
        let reason = format!(
            "execution reverted, data: \
             {}0000000000000000000000000000000000000000000000000000000000000042",
            selector_hex_of::<AckiNackiBridge::UnknownAnchor>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Transient,
        );
    }

    #[test]
    fn classify_layer_out_of_range_is_permanent() {
        use alloy::sol_types::SolError;
        let out_of_range = AckiNackiBridge::LayerOutOfRange {
            layer: 255,
        }
        .abi_encode();
        let reason = format!("execution reverted: 0x{}", hex::encode(out_of_range));
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Permanent,
        );
        assert_eq!(
            decode_withdraw_revert(&reason).unwrap().rendered,
            "LayerOutOfRange(layer: 255)"
        );
    }

    #[test]
    fn classify_treasury_shortfall_is_transient() {
        let reason = format!(
            "execution reverted: {}",
            selector_hex_of::<AckiNackiBridge::WithdrawTreasuryShortfall>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Transient,
        );
    }

    #[test]
    fn a_full_withdrawal_window_is_retried_and_names_the_limit() {
        use alloy::sol_types::SolError;
        assert_eq!(AckiNackiBridge::WithdrawalWindowFull::SELECTOR, [
            0x46, 0x53, 0xf3, 0x81
        ]);
        let full = AckiNackiBridge::WithdrawalWindowFull {
            tokenId: 3,
            used: U256::from(900u64),
            amount: U256::from(200u64),
            dailyLimit: U256::from(1_000u64),
        }
        .abi_encode();
        let reason = format!("execution reverted, data: \"0x{}\"", hex::encode(full));
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Transient
        );
        assert_eq!(
            decode_withdraw_revert(&reason).unwrap().rendered,
            "WithdrawalWindowFull(tokenId: 3, used: 900, amount: 200, dailyLimit: 1000)"
        );
    }

    #[test]
    fn a_successor_before_the_minter_handover_is_retried() {
        use alloy::sol_types::SolError;
        let not_minter = AckiNackiBridge::NacklNotMinter {
            token: alloy::primitives::Address::repeat_byte(0x11),
        }
        .abi_encode();
        let reason = format!("execution reverted: 0x{}", hex::encode(not_minter));
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Transient
        );
        assert_eq!(
            decode_withdraw_revert(&reason).unwrap().name,
            "NacklNotMinter"
        );
    }

    #[test]
    fn a_bridge_that_handed_the_minter_over_parks_the_proof() {
        let reason = format!(
            "execution reverted: {}",
            selector_hex_of::<AckiNackiBridge::NacklMinterGone>()
        );
        assert_eq!(
            classify_withdraw_revert(&reason),
            WithdrawRevertKind::Permanent
        );
        assert_eq!(
            decode_withdraw_revert(&reason).unwrap().rendered,
            "NacklMinterGone()"
        );
    }

    #[test]
    fn classify_rpc_error_defaults_to_transient() {
        // No selector visible => not a proof-intrinsic failure; keep the
        // pre-classification behaviour (backoff-and-retry).
        assert_eq!(
            classify_withdraw_revert("nonce too low"),
            WithdrawRevertKind::Transient,
        );
        assert_eq!(
            classify_withdraw_revert("tx confirmation error: dropped from mempool"),
            WithdrawRevertKind::Transient,
        );
    }

    fn rotation(seq: u64, last_seen: u64, old: U256, new: U256) -> BkSetUpdateData {
        BkSetUpdateData {
            fin_type: FinalizationType::Primary,
            block_id: U256::from(seq),
            block_seq_no: seq,
            attestation_last_seen: last_seen,
            old_commitment_l2: old,
            new_commitment_l3: new,
            sibling_h01: [0u8; 32],
            sibling_h4_7: [0u8; 32],
            sibling_h8_15: [0u8; 32],
            attestation_proof: Bytes::from(vec![0u8; 32]),
        }
    }

    #[tokio::test]
    async fn mock_second_rotation_reverts_until_previous_is_covered() {
        let genesis = U256::from(0xBE5E7u64);
        let next = U256::from(0xC0FFEEu64);
        let third = U256::from(0xD00Du64);
        let bridge = MockBridgeClient::with_genesis(genesis, U256::ZERO, always_accept());

        match bridge
            .submit_bk_set_update(&rotation(100, 0, genesis, next))
            .await
            .unwrap()
        {
            BkSetUpdateSubmitOutcome::Applied {
                new_state, ..
            } => {
                assert_eq!(new_state.last_bk_set_update_seq_no, 100);
                assert_eq!(new_state.last_seen_block_seq_no, 0);
            },
            other => panic!("first rotation must apply ahead of cursor, got {other:?}"),
        }

        match bridge
            .submit_bk_set_update(&rotation(200, 0, next, third))
            .await
            .unwrap()
        {
            BkSetUpdateSubmitOutcome::Reverted {
                reason,
            } => {
                assert!(reason.contains("PrevSlotStillOwedForBundle"), "{reason}");
            },
            other => panic!("second rotation must revert while N1 is uncovered, got {other:?}"),
        }

        let mut cover = block(100, U256::ZERO);
        cover.bk_set_commitment = genesis;
        match bridge.submit_block(&cover).await.unwrap() {
            SubmitOutcome::Verified {
                new_state, ..
            } => {
                assert_eq!(new_state.last_seen_block_seq_no, 100);
            },
            other => panic!("cover verifyBlock failed: {other:?}"),
        }

        match bridge
            .submit_bk_set_update(&rotation(200, 0, next, third))
            .await
            .unwrap()
        {
            BkSetUpdateSubmitOutcome::Applied {
                new_state, ..
            } => {
                assert_eq!(new_state.last_bk_set_update_seq_no, 200);
            },
            other => panic!("second rotation must apply after cover, got {other:?}"),
        }
    }

    /// The snapshot costs the same requests at any age of the bridge: the
    /// head, its hash twice, five scalars and ten windows, and no log.
    #[tokio::test]
    async fn read_full_state_reads_no_logs() {
        use alloy::providers::ProviderBuilder;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let methods = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&methods);
        // One anchor, as `getLayerWindow` encodes it: 128 data words, 128
        // height words, dataLen, writeCursor, lastHeight.
        let window = {
            let mut words = vec![U256::ZERO; 128 + 128 + 3];
            words[0] = U256::from(7u64);
            words[256] = U256::from(1u64);
            words[257] = U256::from(1u64);
            words[258] = U256::from(5u64);
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes::<32>()).collect();
            format!("0x{}", alloy::primitives::hex::encode(bytes))
        };
        let window_selector = format!(
            "0x{}",
            alloy::primitives::hex::encode(AckiNackiBridge::getLayerWindowCall::SELECTOR)
        );
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                let window = window.clone();
                let window_selector = window_selector.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    loop {
                        let mut chunk = [0u8; 4096];
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let Some((head, body)) = text.split_once("\r\n\r\n") else {
                            continue;
                        };
                        let len = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if body.len() < len {
                            continue;
                        }
                        let req: serde_json::Value = serde_json::from_str(&body[..len]).unwrap();
                        buf.clear();
                        let method = req["method"].as_str().unwrap_or("").to_string();
                        let result = match method.as_str() {
                            "eth_blockNumber" => serde_json::json!("0x10"),
                            "eth_getBlockByNumber" => serde_json::json!({
                                "number": "0x10",
                                "hash": format!("0x{}", "11".repeat(32)),
                            }),
                            "eth_call" => {
                                let call = &req["params"][0];
                                let input = call["input"]
                                    .as_str()
                                    .or_else(|| call["data"].as_str())
                                    .unwrap_or("");
                                if input.starts_with(&window_selector) {
                                    serde_json::json!(window)
                                } else {
                                    serde_json::json!(format!("0x{}", "00".repeat(32)))
                                }
                            },
                            _ => serde_json::Value::Null,
                        };
                        seen.lock().unwrap().push(method);
                        let answer = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req["id"],
                            "result": result,
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
                             {}\r\n\r\n{answer}",
                            answer.len()
                        );
                        if sock.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
        let state = EthBridgeClient::new(Address::repeat_byte(1), provider)
            .read_full_state()
            .await
            .unwrap();

        assert_eq!(state.layer_windows[0].data_len, 1);
        assert_eq!(state.layer_windows[0].data[0][0], 7, "slots are LE");
        assert_eq!(state.layer_windows[0].heights[0], 0);
        let methods = methods.lock().unwrap();
        assert!(!methods.iter().any(|m| m == "eth_getLogs"), "{methods:?}");
        assert_eq!(methods.iter().filter(|m| *m == "eth_call").count(), 15);
        assert_eq!(methods.len(), 18, "{methods:?}");
    }

    /// The text a public Sepolia RPC returned for the dry run of a withdrawal
    /// whose nullifier was already spent.
    const NULLIFIER_REVERT: &str =
        "server returned an error response: error code 3: execution reverted: tried to decode \
         error from abi but failed: Error: Failed to slice index abi_decoded_error_data when \
         getting message from revert data, data: \
         \"0xdc215c0a2f01348a22bd5a63a9d567a5f79ac2d0b67b8255981eb11758b080580b9a2fbb\"";

    #[test]
    fn a_spent_nullifier_is_named_with_its_argument() {
        let r = decode_withdraw_revert(NULLIFIER_REVERT).expect("the bridge's own error");
        assert_eq!(r.name, "NullifierAlreadyUsed");
        assert_eq!(
            r.rendered,
            "NullifierAlreadyUsed(nullifier: \
             0x2f01348a22bd5a63a9d567a5f79ac2d0b67b8255981eb11758b080580b9a2fbb)"
        );
        // The daemon's classification reads the same text and must not change.
        assert_eq!(
            classify_withdraw_revert(NULLIFIER_REVERT),
            WithdrawRevertKind::Permanent
        );
    }

    #[test]
    fn revert_data_is_found_wherever_the_node_put_it() {
        use alloy::sol_types::SolError;
        let paused = hex::encode(AckiNackiBridge::BridgePaused::SELECTOR);
        // Inline after the message, no `data:` key, and an address before it
        // that must not be taken for the error.
        let inline = format!(
            "execution reverted: from 0x400bb825ca9ca7dddfcb25f39c1ec073a91246b3: 0x{paused}"
        );
        assert_eq!(
            decode_withdraw_revert(&inline).map(|r| r.rendered),
            Some("BridgePaused()".to_string())
        );
        let shortfall = AckiNackiBridge::WithdrawTreasuryShortfall {
            requested: U256::from(1_000_000u64),
            available: U256::from(20u64),
        }
        .abi_encode();
        let quoted = format!("custom error: data: \"0x{}\"", hex::encode(shortfall));
        assert_eq!(
            decode_withdraw_revert(&quoted).map(|r| r.rendered),
            Some("WithdrawTreasuryShortfall(requested: 1000000, available: 20)".to_string())
        );
    }

    #[test]
    fn text_without_a_bridge_error_decodes_to_nothing() {
        assert_eq!(
            decode_withdraw_revert("error sending request: connection refused"),
            None
        );
        // `Error(string)`: a revert raised by something other than the bridge.
        let foreign =
            "execution reverted, data: \
             \"0x08c379a00000000000000000000000000000000000000000000000000000000000000020\"";
        assert_eq!(decode_withdraw_revert(foreign), None);
    }

    #[test]
    fn every_error_withdraw_by_proof_can_raise_is_decodable() {
        use alloy::{primitives::Address, sol_types::SolError};
        use AckiNackiBridge as B;
        let one = U256::from(1u64);
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (B::BridgePaused {}.abi_encode(), "BridgePaused"),
            (B::Reentrancy {}.abi_encode(), "Reentrancy"),
            (
                B::WithdrawByProofDisabled {}.abi_encode(),
                "WithdrawByProofDisabled",
            ),
            (
                B::WithdrawIdentityMismatch {}.abi_encode(),
                "WithdrawIdentityMismatch",
            ),
            (
                B::DstChainIdMismatch {
                    supplied: one,
                    expected: one,
                }
                .abi_encode(),
                "DstChainIdMismatch",
            ),
            (
                B::WrongDstBridge {
                    dstBridge: one,
                }
                .abi_encode(),
                "WrongDstBridge",
            ),
            (
                B::UnsupportedTokenId {
                    tokenId: one,
                }
                .abi_encode(),
                "UnsupportedTokenId",
            ),
            (
                B::RecipientHalfOutOfRange {
                    value: one,
                }
                .abi_encode(),
                "RecipientHalfOutOfRange",
            ),
            (B::InvalidRecipient {}.abi_encode(), "InvalidRecipient"),
            (
                B::FieldElementOutOfRange {
                    value: one,
                }
                .abi_encode(),
                "FieldElementOutOfRange",
            ),
            (
                B::NullifierAlreadyUsed {
                    nullifier: one,
                }
                .abi_encode(),
                "NullifierAlreadyUsed",
            ),
            (
                B::LayerOutOfRange {
                    layer: 11,
                }
                .abi_encode(),
                "LayerOutOfRange",
            ),
            (
                B::UnknownAnchor {
                    finalRoot: one,
                }
                .abi_encode(),
                "UnknownAnchor",
            ),
            (
                B::WithdrawalProofRejected {}.abi_encode(),
                "WithdrawalProofRejected",
            ),
            (
                B::WithdrawTreasuryShortfall {
                    requested: one,
                    available: one,
                }
                .abi_encode(),
                "WithdrawTreasuryShortfall",
            ),
            (
                B::AaveWithdrawFailed {
                    requested: one,
                    received: one,
                }
                .abi_encode(),
                "AaveWithdrawFailed",
            ),
            (
                B::InsufficientTreasury {}.abi_encode(),
                "InsufficientTreasury",
            ),
            (
                B::WithdrawTransferFailed {
                    recipient: Address::repeat_byte(7),
                    amount: one,
                }
                .abi_encode(),
                "WithdrawTransferFailed",
            ),
        ];
        for (data, name) in cases {
            let text = format!("data: \"0x{}\"", hex::encode(&data));
            assert_eq!(
                decode_withdraw_revert(&text).map(|r| r.name),
                Some(name),
                "{text}"
            );
        }
    }
}
