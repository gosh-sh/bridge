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
    rpc::types::Filter,
    sol_types::SolEvent,
    transports::TransportError,
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

/// Environment variable behind `--bridge-deploy-block` on both entry points
/// (`relayer daemon-live`, `ackinacki-bridge withdraw`); same name as the
/// deposit relayer so one operator env covers both daemons. Named in the
/// messages that ask the operator to check it.
pub const BRIDGE_DEPLOY_BLOCK_ENV: &str = "BRIDGE_DEPLOY_BLOCK";

/// Inclusive `eth_getLogs` span per request. Alchemy free-tier is 10;
/// paid RPC is typically 2_000. The scan walks backwards from the head and
/// stops once every non-empty window's entries are covered, or at
/// [`BRIDGE_DEPLOY_BLOCK_ENV`]; the stop point is the oldest entry still
/// held by any window. Layer N is appended at `128^N` boundaries, so at
/// anchor level 2 layers 1 and 2 fill in about 8 days, while layer 3 gets
/// its first entry at the first `128^3` boundary after the seed (hours to
/// days) and is full only after about 2.8 years: for most of a bridge's
/// life the walk ends at its first layer-3 anchor, in practice near the
/// deploy block. The cost is about `(head - deploy_block) / span` calls, a
/// few per day of bridge age on a 2_000 span and about 720 per day (three
/// minutes a day at 4 calls/s) on a 10-block cap. It runs at `daemon-live`
/// startup, and in the CLI in preflight and after each coverage poll that
/// observed the target.
pub const GET_LOGS_CHUNK_BLOCKS: u64 = 2_000;

/// Attempts for one `eth_getLogs` call of that scan on a retryable RPC
/// error (429, 5xx, transport), with a doubling delay starting at half a
/// second between them. A span-cap rejection (`-32600` / `-32602`) is not
/// retried here: on any chunk but the newest it fails the same way again; on
/// the newest chunk, whose end is the pinned head a lagging backend may not
/// have yet, it re-pins and re-reads the snapshot instead.
const GET_LOGS_MAX_ATTEMPTS: u32 = 8;
const GET_LOGS_INITIAL_BACKOFF_MS: u64 = 500;
const GET_LOGS_MAX_BACKOFF_MS: u64 = 8_000;
/// Log a progress line every this many `eth_getLogs` chunks.
const GET_LOGS_PROGRESS_EVERY: usize = 200;
/// [`EthBridgeClient::read_full_state`] pins every read to one block; a
/// lagging backend behind a load-balanced RPC may answer "header not found"
/// for it, omit the newest log, or reject the newest span, so the whole
/// snapshot is read up to this many times (a short log set re-walks the
/// whole range each time).
const READ_FULL_STATE_ATTEMPTS: u32 = 3;
const READ_FULL_STATE_RETRY_DELAY_MS: u64 = 2_000;

/// How the `LayerAnchorAppended` scan of [`EthBridgeClient::read_full_state`]
/// walks the chain. Both entry points build it from their clap arguments
/// (`--bridge-deploy-block`, `--get-logs-chunk-blocks`,
/// `--get-logs-pause-ms`, each with an `env =` of the same name); a client
/// from [`EthBridgeClient::new`] carries [`LogScanConfig::default`] and is
/// not meant to scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogScanConfig {
    /// Where the backward scan stops when a window is not covered yet: the
    /// block the bridge was deployed in. `0` means genesis: a successful
    /// scan still stops at the bridge's oldest held anchor, but a short
    /// log set then walks back to genesis before failing; warned about
    /// when it runs.
    pub deploy_block: u64,
    /// Inclusive block span of one `eth_getLogs` call. `0` means
    /// [`GET_LOGS_CHUNK_BLOCKS`].
    pub chunk_blocks: u64,
    /// Pause between two `eth_getLogs` calls.
    pub pause: Duration,
}

impl LogScanConfig {
    /// The settings as the scan uses them: a span of 0 means
    /// [`GET_LOGS_CHUNK_BLOCKS`].
    pub fn normalized(self) -> Self {
        Self {
            chunk_blocks: if self.chunk_blocks == 0 {
                GET_LOGS_CHUNK_BLOCKS
            } else {
                self.chunk_blocks
            },
            ..self
        }
    }
}

impl Default for LogScanConfig {
    /// Genesis, the library span, no pause.
    fn default() -> Self {
        Self {
            deploy_block: 0,
            chunk_blocks: GET_LOGS_CHUNK_BLOCKS,
            pause: Duration::ZERO,
        }
    }
}

/// Whether a failed `eth_getLogs` of the scan is worth retrying. A JSON-RPC
/// error response with code `-32600` (invalid request: how Alchemy rejects
/// a span over its cap) or `-32602` (invalid params) fails the same way on
/// every attempt. Everything else (HTTP 429 / 5xx, transport errors,
/// provider-specific rate-limit codes) is transient.
pub fn get_logs_error_is_retryable(e: &TransportError) -> bool {
    match e {
        TransportError::ErrorResp(payload) => !matches!(payload.code, -32600 | -32602),
        _ => true,
    }
}

/// Inclusive `(from, to)` spans covering `from_block..=to_block`, newest
/// first, produced on demand: the backward scan normally stops after a few
/// of them, and a span list from genesis on a 10-block cap would be over a
/// million tuples.
pub fn chunks_newest_first(
    from_block: u64,
    to_block: u64,
    chunk: u64,
) -> impl Iterator<Item = (u64, u64)> {
    let mut end = if chunk == 0 || from_block > to_block {
        None
    } else {
        Some(to_block)
    };
    std::iter::from_fn(move || {
        let e = end?;
        let start = e.saturating_sub(chunk - 1).max(from_block);
        end = if start > from_block {
            Some(start - 1)
        } else {
            None
        };
        Some((start, e))
    })
}

/// How many spans [`chunks_newest_first`] yields.
pub fn chunk_count(from_block: u64, to_block: u64, chunk: u64) -> usize {
    if chunk == 0 || from_block > to_block {
        return 0;
    }
    ((to_block - from_block) / chunk + 1) as usize
}

/// Whether every window with entries has at least as many logs as entries:
/// the backward scan's stop condition.
pub fn windows_covered(found: &[usize], needed: &[usize]) -> bool {
    needed.iter().zip(found).all(|(&n, &f)| f >= n)
}

/// Flatten chunks visited newest-first (each chunk oldest-first inside)
/// into one oldest-first list.
pub fn chronological(newest_chunk_first: Vec<Vec<AnchorEvent>>) -> Vec<AnchorEvent> {
    newest_chunk_first.into_iter().rev().flatten().collect()
}

/// One `LayerAnchorAppended` log: `(layer, blockHeight, hashValue)`, the
/// hash already in the LE form `BridgeState.layer_windows` stores (see
/// [`EthBridgeClient::read_full_state`] on endianness).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorEvent {
    pub layer: u8,
    pub height: u64,
    pub hash_le: [u8; 32],
}

/// Why [`paint_heights_from_events`] refused a set of logs.
#[derive(Debug, thiserror::Error)]
pub enum PaintError {
    /// Fewer logs than window entries: the scan did not reach far enough
    /// back (the deploy block is wrong), or a backend one block behind
    /// omitted the newest append on a window that is not full yet. A
    /// re-read settles which; after the attempts the message names the
    /// deploy block.
    #[error(
        "layer {layer} has data_len={data_len} but only {found} LayerAnchorAppended logs in the \
         scanned range: is {env} the block the bridge was deployed in?",
        env = BRIDGE_DEPLOY_BLOCK_ENV
    )]
    Short {
        layer: usize,
        data_len: usize,
        found: usize,
    },
    /// A kept log and the window slot it should fill disagree: an append
    /// landed between the two reads, or a backend returned a partial log
    /// set. A re-read resolves it.
    #[error(
        "layer {layer} slot {slot}: LayerAnchorAppended hash {log_hash} (height {height}) != \
         window slot hash {slot_hash} (logs and window snapshot disagree; re-read)"
    )]
    SlotMismatch {
        layer: usize,
        slot: usize,
        height: u64,
        log_hash: String,
        slot_hash: String,
    },
    /// Every slot agrees but the window's `lastHeight` does not. Same
    /// causes as [`PaintError::SlotMismatch`]; a re-read resolves it.
    #[error(
        "layer {layer}: newest LayerAnchorAppended height {newest} != on-chain lastHeight \
         {last_height} (logs and window snapshot disagree; re-read)"
    )]
    LastHeight {
        layer: usize,
        newest: u64,
        last_height: u64,
    },
    /// `HistoryWindow::apply_chronological_heights` refused the slice. Not
    /// reachable with exactly `data_len` heights; kept so nothing panics.
    #[error("layer {layer}: {reason}")]
    Shape { layer: usize, reason: String },
}

impl PaintError {
    /// `true` when a fresh snapshot can succeed where this one failed.
    pub fn is_transient(&self) -> bool {
        !matches!(self, PaintError::Shape { .. })
    }
}

impl From<PaintError> for RelayerError {
    fn from(e: PaintError) -> Self {
        RelayerError::other(e.to_string())
    }
}

/// Paint `windows[layer-1].heights` from chronological `LayerAnchorAppended`
/// events (oldest first). Keeps the last `data_len` events per layer,
/// matching `_appendLayer` ring order, and checks each kept event against
/// the slot it fills: `_appendLayer` writes `data[writeCursor] = hashValue`
/// and `lastHeight = blockHeight` from the same values it emits, so a hash
/// that differs, or a newest height that is not `lastHeight`, means the
/// logs and the window snapshot are from different blocks (or the log set
/// is partial) and every height would land one slot off. Empty windows are
/// left alone.
pub fn paint_heights_from_events(
    windows: &mut [HistoryWindow],
    events: &[AnchorEvent],
) -> Result<(), PaintError> {
    let mut by_layer: Vec<Vec<AnchorEvent>> = vec![Vec::new(); MAX_LAYER_HASHES];
    for ev in events {
        if ev.layer == 0 || (ev.layer as usize) > MAX_LAYER_HASHES {
            continue;
        }
        by_layer[(ev.layer as usize) - 1].push(*ev);
    }
    for (idx, window) in windows.iter_mut().enumerate() {
        if window.data_len == 0 {
            continue;
        }
        let evs = &by_layer[idx];
        if evs.len() < window.data_len {
            return Err(PaintError::Short {
                layer: idx + 1,
                data_len: window.data_len,
                found: evs.len(),
            });
        }
        let oldest_first = &evs[evs.len() - window.data_len..];
        for (slot, ((slot_hash, _), ev)) in
            window.iter_chronological().zip(oldest_first).enumerate()
        {
            if slot_hash != ev.hash_le {
                return Err(PaintError::SlotMismatch {
                    layer: idx + 1,
                    slot,
                    height: ev.height,
                    log_hash: hex_le(&ev.hash_le),
                    slot_hash: hex_le(&slot_hash),
                });
            }
        }
        if let Some(newest) = oldest_first.last() {
            if newest.height != window.last_height {
                return Err(PaintError::LastHeight {
                    layer: idx + 1,
                    newest: newest.height,
                    last_height: window.last_height,
                });
            }
        }
        let heights: Vec<u64> = oldest_first.iter().map(|ev| ev.height).collect();
        window
            .apply_chronological_heights(&heights)
            .map_err(|e| PaintError::Shape {
                layer: idx + 1,
                reason: e.to_string(),
            })?;
    }
    Ok(())
}

/// `0x…` rendering of a 32-byte LE slot for messages.
fn hex_le(bytes: &[u8; 32]) -> String {
    format!("0x{}", alloy::primitives::hex::encode(bytes))
}

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
                prev_bk_set_commitment: U256::ZERO,
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
        if inner.last_bk_set_update_seq_no > inner.last_seen_block_seq_no {
            return Ok(BkSetUpdateSubmitOutcome::Reverted {
                reason: format!(
                    "VerifyBlockLagBehindRotation(rotation={}, last_seen={})",
                    inner.last_bk_set_update_seq_no, inner.last_seen_block_seq_no
                ),
            });
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
            }

            function withdrawByProof(
                bytes calldata proof,
                WithdrawalPublicInputs calldata pub
            ) external returns (bool success);

            function isNullifierUsed(uint256 nullifier) external view returns (bool);

            /// Custom errors `AckiNackiBridge.withdrawByProof` can revert with.
            /// Listed here so the sol! macro emits `SELECTOR` constants — the
            /// relayer withdraw scan uses them to distinguish permanent
            /// (proof-intrinsic) reverts from transient ones (e.g. anchor
            /// not yet registered by the verifyBlock lane) so a permanently
            /// bad proof does not hold up the whole queue.
            error WithdrawByProofDisabled();
            error WithdrawalProofRejected();
            error NullifierAlreadyUsed(uint256 nullifier);
            error FieldElementOutOfRange(uint256 value);
            error DstChainIdMismatch(uint256 supplied, uint256 expected);
            error RecipientHalfOutOfRange(uint256 value);
            error WithdrawIdentityMismatch();
            error UnknownAnchor(uint256 finalRoot);
            error InvalidNumLayers(uint256 anchorLayer);
            error UnsupportedTokenId(uint256 tokenId);
            error InvalidRecipient();
            error WithdrawTreasuryShortfall(uint256 requested, uint256 available);

            /// Read-only getters the end-user CLI preflights the deploy
            /// with, before the irreversible Acki Nacki burn. All four are
            /// plain public state / immutables on `AckiNackiBridge`.
            function treasuryBalance() external view returns (uint256);
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
        /// `deploy/shellnet-l2/scripts/preflight.sh:28` does: a non-zero
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

use sol_bindings::AckiNackiBridge;

/// Production bridge client — wraps `sol!`-generated bindings.
///
/// Generic over any alloy [`Provider`] (a wallet-filled provider in
/// production; a plain HTTP provider in read-only smoke tests).
pub struct EthBridgeClient<P: Provider<N>, N: Network = alloy::network::Ethereum> {
    contract: AckiNackiBridge::AckiNackiBridgeInstance<P, N>,
    address: Address,
    /// How the startup `LayerAnchorAppended` scan walks the chain:
    /// [`LogScanConfig::default`] from [`Self::new`], explicit in
    /// [`Self::with_scan_config`]. Set `deploy_block` in production so a
    /// failed scan does not walk back to genesis.
    scan: LogScanConfig,
}

impl<P, N> EthBridgeClient<P, N>
where
    P: Provider<N> + Clone,
    N: Network,
{
    /// A client for calls that never rebuild window heights. It carries
    /// [`LogScanConfig::default`], so a `read_full_state` on it would walk
    /// back to genesis (warned about); clients that scan come from
    /// [`Self::with_scan_config`].
    pub fn new(address: Address, provider: P) -> Self {
        Self::with_scan_config(address, provider, LogScanConfig::default())
    }

    /// `scan.chunk_blocks == 0` means [`GET_LOGS_CHUNK_BLOCKS`]
    /// ([`LogScanConfig::normalized`]).
    pub fn with_scan_config(address: Address, provider: P, scan: LogScanConfig) -> Self {
        let contract = AckiNackiBridge::new(address, provider);
        Self {
            contract,
            address,
            scan: scan.normalized(),
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

    /// One `eth_getLogs` for `LayerAnchorAppended` over the newest
    /// `chunk_blocks` blocks: the call the scan would send first. Lets a
    /// preflight exercise the RPC's span cap and log serving on a bridge
    /// whose windows are still empty, where the scan itself sends nothing.
    /// Same tolerance as the scan's newest chunk: retryable RPC errors are
    /// retried with backoff, and a rejected span (the head may be one a
    /// lagging backend does not have yet) is re-pinned a few times before
    /// the error surfaces. Returns the number of logs in that span.
    pub async fn probe_log_span(&self) -> Result<usize, RelayerError> {
        let mut pin = 0u32;
        loop {
            pin += 1;
            let outcome = self.probe_log_span_once().await;
            match outcome {
                Ok(n) => return Ok(n),
                Err(e) if pin < READ_FULL_STATE_ATTEMPTS => {
                    tracing::warn!(
                        pin,
                        error = %e,
                        "LayerAnchorAppended probe failed; re-pinning the head"
                    );
                    tokio::time::sleep(Duration::from_millis(READ_FULL_STATE_RETRY_DELAY_MS)).await;
                },
                Err(e) => return Err(e),
            }
        }
    }

    async fn probe_log_span_once(&self) -> Result<usize, RelayerError> {
        let head = self
            .contract
            .provider()
            .get_block_number()
            .await
            .map_err(|e| RelayerError::other(format!("get_block_number: {e}")))?;
        let from = head.saturating_sub(self.scan.chunk_blocks.saturating_sub(1));
        let filter = Filter::new()
            .address(self.address)
            .event_signature(AckiNackiBridge::LayerAnchorAppended::SIGNATURE_HASH)
            .from_block(from)
            .to_block(head);
        let mut attempt = 0u32;
        let mut backoff_ms = GET_LOGS_INITIAL_BACKOFF_MS;
        loop {
            attempt += 1;
            match self.contract.provider().get_logs(&filter).await {
                Ok(logs) => return Ok(logs.len()),
                Err(e) if attempt < GET_LOGS_MAX_ATTEMPTS && get_logs_error_is_retryable(&e) => {
                    tracing::warn!(
                        from,
                        head,
                        attempt,
                        backoff_ms,
                        error = %e,
                        "LayerAnchorAppended probe failed; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = backoff_ms.saturating_mul(2).min(GET_LOGS_MAX_BACKOFF_MS);
                },
                Err(e) => {
                    return Err(RelayerError::other(format!(
                        "LayerAnchorAppended get_logs [{from},{head}] after {attempt} attempt(s): \
                         {e}"
                    )));
                },
            }
        }
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
                Err(e) => {
                    // Post-send confirmation failures (RPC dropped, receipt
                    // wait timed out) are transient by nature — the tx
                    // may still land or the RPC may recover — so we do
                    // not park the proof here.
                    let reason = format!("tx confirmation error: {e}");
                    Ok(WithdrawSubmitOutcome::Reverted {
                        reason,
                        permanent: false,
                    })
                },
            },
            Err(e) => {
                let reason = format!("withdrawByProof send failed: {e}");
                let permanent = classify_withdraw_revert(&reason) == WithdrawRevertKind::Permanent;
                Ok(WithdrawSubmitOutcome::Reverted {
                    reason,
                    permanent,
                })
            },
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
            update.attestation_last_seen,
            update.old_commitment_l2,
            update.new_commitment_l3,
            B256::from(update.sibling_h01),
            B256::from(update.sibling_h4_7),
            B256::from(update.sibling_h8_15),
        );
        match call.send().await {
            Ok(pending) => match pending.get_receipt().await {
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
    /// to one block, then scans `LayerAnchorAppended` backwards from that
    /// block to paint the per-slot heights (the contract no longer stores
    /// them): see `paint_heights_from_appended_logs` for the walk and
    /// [`GET_LOGS_CHUNK_BLOCKS`] for its cost. Each window returns ~5 KB, so
    /// the view calls cost ~50 KB of RPC response — well under any
    /// provider's per-call cap.
    ///
    /// Consistency: `eth_blockNumber` is taken first and every read is
    /// pinned to it, so a `verifyBlock` landing mid-read cannot leave the
    /// windows one append ahead of the logs. [`paint_heights_from_events`]
    /// then checks every kept log against its window slot (hash and
    /// height) and the window's `lastHeight`, so a partial log set from a
    /// lagging backend is reported instead of mis-painted. A failed
    /// snapshot is re-read a few times whenever a fresh one can succeed: a
    /// pinned read the backend cannot serve yet, a slot or `lastHeight`
    /// mismatch, fewer logs than entries (a backend one block behind; each
    /// re-read is a full walk), and a rejected newest span, which on a
    /// deterministic span cap costs two extra short reads before the same
    /// refusal. A chunk below the newest that the RPC rejected or that ran
    /// out of its attempts returns at once: re-reading would repeat the
    /// whole walk.
    ///
    /// Callers: `daemon-live` once at startup; the withdrawal CLI in
    /// preflight and after each coverage poll (one scalar call per round)
    /// that observed the target.
    pub async fn read_full_state(&self) -> Result<EthBridgeContractState, RelayerError> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.read_full_state_once().await {
                Ok(state) => return Ok(state),
                Err(SnapshotError::Transient(e)) if attempt < READ_FULL_STATE_ATTEMPTS => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "read_full_state failed; retrying the pinned snapshot"
                    );
                    tokio::time::sleep(Duration::from_millis(READ_FULL_STATE_RETRY_DELAY_MS)).await;
                },
                Err(SnapshotError::Transient(e)) | Err(SnapshotError::Final(e)) => return Err(e),
            }
        }
    }

    async fn read_full_state_once(&self) -> Result<EthBridgeContractState, SnapshotError> {
        // Everything up to the scan is one pinned read set: what fails here
        // is the RPC not serving the pinned block yet, or not answering.
        let head = self
            .contract
            .provider()
            .get_block_number()
            .await
            .map_err(|e| {
                SnapshotError::Transient(RelayerError::other(format!("get_block_number: {e}")))
            })?;
        let at = BlockId::from(head);
        let scalars = self
            .read_state_at(at)
            .await
            .map_err(SnapshotError::Transient)?;

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
                .map_err(map_contract_err)
                .map_err(SnapshotError::Transient)?;
            let data: Vec<[u8; 32]> = w
                .data
                .iter()
                .map(|u| {
                    let mut le = u.to_be_bytes::<32>();
                    le.reverse();
                    le
                })
                .collect();
            // On-chain `heights` are no longer written. The ring is painted
            // below from `LayerAnchorAppended` logs.
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
        self.paint_heights_from_appended_logs(&mut windows, head)
            .await?;
        let layer_windows: [HistoryWindow; MAX_LAYER_HASHES] =
            windows.try_into().map_err(|_| {
                SnapshotError::Final(RelayerError::Other(
                    "read_full_state: expected 10 layer windows".into(),
                ))
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

    /// Fill each window's `heights` from `LayerAnchorAppended` logs (the
    /// contract no longer SSTOREs them). The walk goes backwards from `to`
    /// in [`LogScanConfig::chunk_blocks`]-block `eth_getLogs` calls and
    /// stops as soon as every non-empty window has at least `data_len`
    /// logs, or at the deploy block. The stop point is the oldest entry
    /// still held by any non-empty window: layer N is appended at `128^N`
    /// boundaries, so a layer that is not full (layer 3 for about 2.8
    /// years) keeps its very first entry and the walk reaches back to it,
    /// in practice near the deploy block; see [`GET_LOGS_CHUNK_BLOCKS`] for
    /// the cost. The logs are then put back in chronological order and
    /// [`paint_heights_from_events`] keeps the last `data_len` per layer.
    async fn paint_heights_from_appended_logs(
        &self,
        windows: &mut [HistoryWindow],
        to: u64,
    ) -> Result<(), SnapshotError> {
        let needed: Vec<usize> = windows.iter().map(|w| w.data_len).collect();
        if needed.iter().all(|&n| n == 0) {
            return Ok(());
        }
        // Without from/to, eth_getLogs defaults both to `latest` and
        // returns one block. Resurrect then fails
        // `data_len=X but only 0 LayerAnchorAppended logs` (ETH-31).
        if self.scan.deploy_block == 0 {
            tracing::warn!(
                "{BRIDGE_DEPLOY_BLOCK_ENV} is unset: the LayerAnchorAppended scan may walk back \
                 to genesis; set it to the block the bridge was deployed in"
            );
        }
        let from = self.scan.deploy_block.min(to);
        let total = chunk_count(from, to, self.scan.chunk_blocks);
        let started = std::time::Instant::now();
        tracing::info!(
            from,
            to,
            chunk_blocks = self.scan.chunk_blocks,
            pause_ms = self.scan.pause.as_millis() as u64,
            chunks = total,
            "scanning LayerAnchorAppended backwards from the head"
        );
        let mut newest_chunk_first: Vec<Vec<AnchorEvent>> = Vec::new();
        let mut found = vec![0usize; MAX_LAYER_HASHES];
        let mut read = 0usize;
        let mut covered = false;
        for (start, end) in chunks_newest_first(from, to, self.scan.chunk_blocks) {
            let filter = Filter::new()
                .address(self.address)
                .event_signature(AckiNackiBridge::LayerAnchorAppended::SIGNATURE_HASH)
                .from_block(start)
                .to_block(end);
            let mut attempt = 0u32;
            let mut backoff_ms = GET_LOGS_INITIAL_BACKOFF_MS;
            let logs = loop {
                attempt += 1;
                match self.contract.provider().get_logs(&filter).await {
                    Ok(logs) => break logs,
                    Err(e)
                        if attempt < GET_LOGS_MAX_ATTEMPTS && get_logs_error_is_retryable(&e) =>
                    {
                        tracing::warn!(
                            start,
                            end,
                            attempt,
                            backoff_ms,
                            error = %e,
                            "LayerAnchorAppended get_logs failed; retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = backoff_ms.saturating_mul(2).min(GET_LOGS_MAX_BACKOFF_MS);
                    },
                    Err(e) => {
                        let err = RelayerError::other(format!(
                            "LayerAnchorAppended get_logs [{start},{end}] after {attempt} \
                             attempt(s): {e}"
                        ));
                        // The newest chunk ends at the pinned head, which a
                        // lagging backend may not have yet: re-pin and
                        // re-read. Any other chunk fails the same way again.
                        return Err(if end == to {
                            SnapshotError::Transient(err)
                        } else {
                            SnapshotError::Final(err)
                        });
                    },
                }
            };
            read += 1;
            if !self.scan.pause.is_zero() {
                tokio::time::sleep(self.scan.pause).await;
            }
            let mut chunk_events = Vec::new();
            for log in logs {
                let decoded = match log.log_decode::<AckiNackiBridge::LayerAnchorAppended>() {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let ev = decoded.inner.data;
                let mut hash_le = ev.hashValue.to_be_bytes::<32>();
                hash_le.reverse();
                if ev.layer != 0 && (ev.layer as usize) <= MAX_LAYER_HASHES {
                    found[(ev.layer as usize) - 1] += 1;
                }
                chunk_events.push(AnchorEvent {
                    layer: ev.layer,
                    height: ev.blockHeight,
                    hash_le,
                });
            }
            if !chunk_events.is_empty() {
                newest_chunk_first.push(chunk_events);
            }
            if windows_covered(&found, &needed) {
                covered = true;
                break;
            }
            if read.is_multiple_of(GET_LOGS_PROGRESS_EVERY) {
                tracing::info!(
                    read,
                    chunks = total,
                    down_to_block = start,
                    events = found.iter().sum::<usize>(),
                    elapsed_s = started.elapsed().as_secs(),
                    "LayerAnchorAppended scan progress"
                );
            }
        }
        let events = chronological(newest_chunk_first);
        tracing::info!(
            read,
            chunks = total,
            covered,
            events = events.len(),
            elapsed_s = started.elapsed().as_secs(),
            "LayerAnchorAppended scan done"
        );
        paint_heights_from_events(windows, &events).map_err(|e| {
            let transient = e.is_transient();
            let err = RelayerError::other(format!(
                "{e} (scanned blocks {from}..={to}; {read} of {total} chunks read)"
            ));
            if transient {
                SnapshotError::Transient(err)
            } else {
                SnapshotError::Final(err)
            }
        })
    }
}

/// Classification of a failed [`EthBridgeClient::read_full_state`]: whether
/// a fresh snapshot can succeed where this one failed.
#[derive(Debug)]
enum SnapshotError {
    /// A pinned read the backend could not serve yet, logs and window
    /// snapshot that disagree, fewer logs than entries (re-walked in full),
    /// or a rejected newest span: re-read.
    Transient(RelayerError),
    /// A chunk below the newest that the RPC rejected or that ran out of
    /// its attempts, or a window shape the ring cannot take: re-reading
    /// would repeat the whole walk.
    Final(RelayerError),
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
    let permanent_selectors: [[u8; 4]; 9] = [
        AckiNackiBridge::WithdrawalProofRejected::SELECTOR,
        AckiNackiBridge::WithdrawIdentityMismatch::SELECTOR,
        AckiNackiBridge::DstChainIdMismatch::SELECTOR,
        AckiNackiBridge::UnsupportedTokenId::SELECTOR,
        AckiNackiBridge::RecipientHalfOutOfRange::SELECTOR,
        AckiNackiBridge::InvalidRecipient::SELECTOR,
        AckiNackiBridge::FieldElementOutOfRange::SELECTOR,
        AckiNackiBridge::InvalidNumLayers::SELECTOR,
        AckiNackiBridge::NullifierAlreadyUsed::SELECTOR,
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
                assert!(reason.contains("VerifyBlockLagBehindRotation"), "{reason}");
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

    #[test]
    fn chunks_newest_first_cover_the_range_backwards() {
        let spans: Vec<(u64, u64)> = chunks_newest_first(100, 350, 100).collect();
        assert_eq!(spans, vec![(251, 350), (151, 250), (100, 150)]);
        assert_eq!(chunk_count(100, 350, 100), 3);
        assert_eq!(chunks_newest_first(5, 5, 10).collect::<Vec<_>>(), vec![(
            5, 5
        )]);
        assert_eq!(chunks_newest_first(10, 9, 10).count(), 0);
        assert_eq!(chunk_count(10, 9, 10), 0);
        assert_eq!(chunks_newest_first(0, 0, 2_000).collect::<Vec<_>>(), vec![
            (0, 0)
        ]);
        assert_eq!(chunks_newest_first(0, 25, 10).collect::<Vec<_>>(), vec![
            (16, 25),
            (6, 15),
            (0, 5)
        ]);
        assert_eq!(chunk_count(0, 25, 10), 3);
    }

    fn ev(layer: u8, height: u64, tag: u8) -> AnchorEvent {
        AnchorEvent {
            layer,
            height,
            hash_le: [tag; 32],
        }
    }

    /// A window whose slots hold `[tag; 32]` hashes with zero heights, the
    /// way `read_full_state` builds it before painting.
    fn window_with(tags: &[u8], last_height: u64) -> HistoryWindow {
        let mut w = HistoryWindow::new(4);
        for &t in tags {
            w.append([t; 32], 0);
        }
        w.last_height = last_height;
        w
    }

    #[test]
    fn paint_heights_keeps_last_data_len_events() {
        let mut windows = vec![window_with(&[2, 3], 30)];
        paint_heights_from_events(&mut windows, &[ev(1, 10, 1), ev(1, 20, 2), ev(1, 30, 3)])
            .unwrap();
        let heights: Vec<u64> = windows[0].iter_chronological().map(|(_, h)| h).collect();
        assert_eq!(heights, vec![20, 30]);
    }

    #[test]
    fn paint_heights_errors_when_logs_are_short() {
        let err = paint_heights_from_events(&mut [window_with(&[1, 2], 10)], &[ev(1, 10, 1)])
            .unwrap_err();
        assert!(
            matches!(err, PaintError::Short {
                layer: 1,
                data_len: 2,
                found: 1
            }),
            "{err}"
        );
        assert!(err.is_transient());
        assert!(err.to_string().contains(BRIDGE_DEPLOY_BLOCK_ENV), "{err}");
    }

    #[test]
    fn paint_heights_errors_when_the_snapshot_is_one_append_behind() {
        // One more event than the window snapshot knows: an append landed
        // between the two reads. Every hash shifts by one slot, which the
        // slot check catches first.
        let err = paint_heights_from_events(&mut [window_with(&[2, 3], 30)], &[
            ev(1, 10, 1),
            ev(1, 20, 2),
            ev(1, 30, 3),
            ev(1, 40, 4),
        ])
        .unwrap_err();
        assert!(
            matches!(err, PaintError::SlotMismatch {
                layer: 1,
                slot: 0,
                height: 30,
                ..
            }),
            "{err}"
        );
        assert!(err.is_transient());
        // Same hashes, stale lastHeight: the height check catches it.
        let err = paint_heights_from_events(&mut [window_with(&[2, 3], 20)], &[
            ev(1, 20, 2),
            ev(1, 30, 3),
        ])
        .unwrap_err();
        assert!(
            matches!(err, PaintError::LastHeight {
                layer: 1,
                newest: 30,
                last_height: 20
            }),
            "{err}"
        );
        assert!(err.is_transient());
    }

    #[test]
    fn paint_heights_errors_on_a_gap_in_the_middle() {
        // Enough logs and the right newest one, but a middle chunk came
        // back empty: slot 0 would get height 10 instead of 20.
        let err = paint_heights_from_events(&mut [window_with(&[2, 3], 30)], &[
            ev(1, 10, 1),
            ev(1, 30, 3),
        ])
        .unwrap_err();
        assert!(
            matches!(err, PaintError::SlotMismatch {
                slot: 0,
                height: 10,
                ..
            }),
            "{err}"
        );
    }

    #[test]
    fn backward_scan_stops_once_windows_are_covered() {
        assert!(windows_covered(&[2, 0, 5], &[2, 0, 3]));
        assert!(!windows_covered(&[1, 0, 5], &[2, 0, 3]));
        assert!(windows_covered(&[0; 10], &[0; 10]));
        let chunks = vec![vec![ev(1, 30, 3)], vec![ev(1, 10, 1), ev(1, 20, 2)]];
        let heights: Vec<u64> = chronological(chunks).iter().map(|e| e.height).collect();
        assert_eq!(heights, vec![10, 20, 30]);
    }

    /// A JSON-RPC error response as the provider surfaces it.
    fn rpc_error_response(code: i64, message: &str) -> TransportError {
        TransportError::ErrorResp(
            serde_json::from_value(serde_json::json!({ "code": code, "message": message }))
                .expect("error payload"),
        )
    }

    #[test]
    fn get_logs_span_cap_is_not_retried() {
        let cap = rpc_error_response(
            -32600,
            "Under the Free tier, up to a 10 block range is supported",
        );
        assert!(!get_logs_error_is_retryable(&cap));
        assert!(!get_logs_error_is_retryable(&rpc_error_response(
            -32602,
            "invalid params"
        )));
        let rate = rpc_error_response(429, "Too Many Requests");
        assert!(get_logs_error_is_retryable(&rate));
        let transport = alloy::transports::TransportErrorKind::custom_str("connection reset");
        assert!(get_logs_error_is_retryable(&transport));
    }
}
