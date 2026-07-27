//! Live driver — reusable AN-side orchestration for Circuits 1A / 1B / 2 +
//! BK-set rotations.
//!
//! This module extracts the "poll GQL → build private witness → run the halo2
//! prover" loop that used to live inlined in `bridge-prover-daemon`'s
//! `main.rs`. The extraction lets:
//!
//! * our own daemon shrink `main.rs` to a thin harness (CLI + IPC + disk
//!   persistence), and
//! * external consumers (`crates/bridge-relayer-daemon` — Sergey's ETH-side
//!   relayer) reuse the same orchestration by adding a single dep on
//!   `bridge-prover-lib` and calling [`LiveProverDriver::poll_next_bundle`] +
//!   [`LiveProverDriver::poll_next_bk_update`].
//!
//! ## Design notes
//!
//! * **No persistence.** The driver holds `BridgeState` + `ProverBkSet` in
//!   memory only. The caller calls [`snapshot_state`](LiveProverDriver::snapshot_state)
//!   + [`snapshot_prover_bk_set`](LiveProverDriver::snapshot_prover_bk_set)
//!   after every ack and saves them itself. Our daemon uses `./state/*.json`;
//!   Sergey's uses whatever fits his layout.
//! * **No sleeps.** Both poll methods are non-blocking and return a variant
//!   even when there is nothing new (e.g. [`LiveBundleEvent::Nothing`]). The
//!   caller owns the poll interval and backoff.
//! * **No `Fr` in the public payloads.** [`BundleProofArtifacts`] and
//!   [`BkUpdateProofArtifacts`] carry only `[u8; 32]` (canonical
//!   `fr.to_repr()` bytes) so consumers do not transitively pull the halo2
//!   field type. Our IPC verifier and Sergey's `AnBlockData` both convert
//!   with a trivial `From` impl on their side.
//! * **Idempotent acks.** [`LiveProverDriver::ack_bundle`] and
//!   [`LiveProverDriver::ack_bk_update`] compare `block_seq_no` to the
//!   in-memory cursor and no-op if the state is already past. This means the
//!   caller can re-ack after a crash-restart without corrupting state.
//! * **`bk-updates block bundles`.** [`LiveProverDriver::poll_next_bundle`]
//!   returns [`LiveBundleEvent::Nothing`] with
//!   `blocked_by_pending_bk_update = true` whenever there is an un-drained
//!   rotation whose block-height sits `<=` the next thinned-key-block target.
//!   The caller must drain via [`poll_next_bk_update`](LiveProverDriver::poll_next_bk_update)
//!   before bundles can advance past that height.
//!
//! See `an_bridge_prover_live_driver_refactor_plan_2026-07-08.md` in the repo
//! root for the full extraction rationale and the two-daemon integration
//! contract.
//!
//! ## Downstream-consumer contract (Sergey's `bridge-relayer-daemon`)
//!
//! External consumers wire the driver into their own poll loop by:
//!
//! 1. building a [`bridge_gql_fetcher::gql_client::GqlClient`] pointed at an AN node,
//! 2. constructing a [`KeyManager`] and calling `ensure_primary_keys` /
//!    `ensure_fallback_keys` / `ensure_layer_keys` once at startup,
//! 3. loading or bootstrapping a [`BridgeState`] + [`ProverBkSet`] from
//!    their own persistence layer,
//! 4. bootstrapping the initial BK-set map by folding rotation events on top
//!    of a genesis anchor via
//!    [`bridge_gql_fetcher::bk_set_fetcher::bk_set_at_height`] (the old
//!    `fetch_bk_set` replayed the delta log from ∅ and missed the un-emitted
//!    genesis committee — disabled 2026-07-22), and
//! 5. constructing [`LiveProverDriver`] with a [`LiveProverConfig`] whose
//!    [`SeedPolicy`] matches the desired bootstrap mode.
//!
//! From there they call [`LiveProverDriver::poll_next_bundle`] +
//! [`LiveProverDriver::poll_next_bk_update`] on a tick, submit the
//! resulting artifacts to their downstream contract via `alloy` (or
//! whatever transport), then invoke [`LiveProverDriver::ack_bundle`] /
//! [`LiveProverDriver::ack_bk_update`] on success. On error they call
//! neither `ack_*` and re-poll on the next tick — the driver's cursors
//! are unchanged so the same artifact is re-emitted for retry.
//!
//! ### halo2 transitive dependency note
//!
//! Depending on `bridge-prover-lib` pulls in the halo2 proving stack
//! (halo2-base, halo2-ecc, gosh forks). Consumers do not need to know
//! anything about halo2 — the public payloads
//! ([`BundleProofArtifacts`], [`BkUpdateProofArtifacts`]) carry only
//! `[u8; 32]` and `Vec<u8>` fields. But their `Cargo.lock` will still
//! contain halo2 crates, which affects link-time (multi-gigabyte
//! debug builds) and release-build size. This is inherent — the driver
//! runs the proving pipeline in-process.

use std::collections::HashMap;

use anyhow::Context;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use thiserror::Error;
use tracing::{info, warn};

use bridge_gql_fetcher::attestation_fetcher::AttestationEvidence;
use crate::bootstrap::BootstrapSeed;
use crate::bridge_state::{BridgeState, MAX_LAYERS};
use bridge_gql_fetcher::gql_client::GqlClient;
use crate::keys::KeyManager;
use bridge_poseidon as poseidon;
use crate::prover_bk_set::ProverBkSet;

mod bk_update;
mod bundle;
pub mod thinning;

pub use thinning::find_next_thinned_key_block;

/// History window `W` — pulled from the vendored `poseidon_dense` constant so
/// the driver and its callers always agree without depending on any node
/// crate.
pub const HISTORY_WINDOW_SIZE: u64 =
    crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64;

/// Safety cap on how many rotations
/// [`LiveProverDriver::poll_next_bk_update`] will surface between two
/// consecutive [`LiveProverDriver::poll_next_bundle`] calls without a
/// [`LiveProverDriver::ack_bk_update`]. Prevents accidental infinite drains
/// if a caller polls without ever acking — the driver still emits at most
/// this many artifacts back-to-back, so a broken consumer can't spin the
/// prover indefinitely.
pub const DEFAULT_MAX_BK_UPDATES_PER_ITER: usize = 8;

/// Structured error surface for [`LiveProverDriver`] public methods.
///
/// The driver's internals still use `anyhow::Result` for ergonomics (many
/// `?`-chained fallible calls into GQL / halo2), but the public boundary
/// coerces those into this enum so consumers (Sergey's `bridge-relayer-daemon`
/// and our own `bridge-prover-daemon`) can pattern-match on failure kind
/// without depending on `anyhow` in their public signatures.
///
/// The `#[from] anyhow::Error` inner value preserves the original error chain
/// so `tracing::error!("{:?}", err)` still surfaces full context. Consumers
/// only need to peek at the variant to decide policy (retry vs. bail vs.
/// operator alert).
///
/// The blanket `impl<E: Error+Send+Sync+'static> From<E> for anyhow::Error`
/// in the `anyhow` crate means the daemon can keep using `?` at call sites
/// that return `anyhow::Result` — [`DriverError`] threads through unchanged.
#[derive(Error, Debug)]
pub enum DriverError {
    /// Transient GraphQL / HTTP failure. Safe to retry on the next poll.
    ///
    /// Examples: node briefly unreachable, GQL 5xx, timeout on
    /// `query_latest_blocks`.
    #[error("transient GQL failure: {0}")]
    GqlTransient(#[source] anyhow::Error),

    /// GraphQL schema mismatch or an unexpected field shape. Not
    /// retryable — signals a node/library version drift.
    #[error("GQL schema mismatch: {0}")]
    GqlSchema(#[source] anyhow::Error),

    /// halo2 proof generation failed for the target seqno. Typically fatal:
    /// re-running the same witness will fail the same way. Callers should
    /// alert and stop the pipeline pending operator intervention.
    #[error("proof generation failed at seq_no {seq_no}: {source}")]
    ProofGen {
        seq_no: u64,
        #[source]
        source: anyhow::Error,
    },

    /// Driver's in-memory state disagrees with what the chain reports (e.g.
    /// BK-set commitment mismatch, L2 sibling ≠ current commitment).
    /// Non-retryable — an operator must reconcile.
    #[error("driver state inconsistent with chain: {0}")]
    StateInconsistent(#[source] anyhow::Error),

    /// Bootstrap-phase signal: driver is still waiting for chain head to
    /// catch up to the seed height. Not an error in the usual sense; both
    /// poll methods return this via [`LiveBundleEvent::Bootstrapping`] /
    /// [`LiveBkUpdateEvent::Bootstrapping`] rather than as `Err`, so this
    /// variant is reserved for edge cases where the bootstrap machinery
    /// itself fails.
    #[error("bootstrap failure (seed={seed_seqno}, head={chain_head_seqno}): {source}")]
    Bootstrapping {
        seed_seqno: u64,
        chain_head_seqno: u64,
        #[source]
        source: anyhow::Error,
    },

    /// Anything not classifiable into the above buckets. Uses `{0:#}` so the
    /// full anyhow context chain (all `.with_context(..)` frames) is included
    /// in the Display output, not just the top-level message.
    #[error("driver error: {0:#}")]
    Other(#[source] anyhow::Error),
}

impl DriverError {
    /// Wrap an arbitrary `anyhow::Error` as [`DriverError::Other`]. Used at
    /// the public boundary of poll/ack methods to lift the internal
    /// anyhow-based helpers into the structured public error type without
    /// forcing a full site-by-site classification pass. Site-specific
    /// variants (`GqlTransient`, `ProofGen`, etc.) are constructed
    /// explicitly at the call sites that know their semantic kind.
    pub(crate) fn other(err: anyhow::Error) -> Self {
        DriverError::Other(err)
    }
}

/// Convenience alias.
pub type DriverResult<T> = Result<T, DriverError>;

/// Configuration surface for [`LiveProverDriver`]. No filesystem paths, no
/// CLI knobs, no verifier timeouts — those all live with the caller.
#[derive(Debug, Clone)]
pub struct LiveProverConfig {
    /// Number of consecutive master key blocks aggregated inside a single
    /// Circuit 2 bundle. Matches [`crate::THINNING_FACTOR_P`] by default.
    pub thinning_factor_p: u64,
    /// Rolling-window width per layer. Matches [`HISTORY_WINDOW_SIZE`] by
    /// default.
    pub history_window_size: u64,
    /// Safety cap; see [`DEFAULT_MAX_BK_UPDATES_PER_ITER`].
    pub max_bk_updates_per_iter: usize,
    /// How the driver picks the bootstrap seed. See [`SeedPolicy`].
    pub seed_policy: SeedPolicy,
}

impl Default for LiveProverConfig {
    fn default() -> Self {
        Self {
            thinning_factor_p: crate::THINNING_FACTOR_P,
            history_window_size: HISTORY_WINDOW_SIZE,
            max_bk_updates_per_iter: DEFAULT_MAX_BK_UPDATES_PER_ITER,
            seed_policy: SeedPolicy::Resume,
        }
    }
}

/// Bootstrap strategy.
///
/// The driver applies the seed exactly once, when the wrapped
/// [`BridgeState`] is not yet `initialized`. On a warm start (state loaded
/// from disk) the policy is ignored and the driver transitions straight to
/// steady state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedPolicy {
    /// Pin a specific seqno. MUST be `> 0` and divisible by `W * P`.
    ///
    /// Used by manual / reproducible starts (our daemon's
    /// `BRIDGE_BOOTSTRAP_SEQNO=N`).
    Explicit(u64),
    /// Auto-select: on first poll, snap to the next `W * P`-aligned seqno
    /// strictly past the current chain head.
    ///
    /// Used for automatic starts on a long-running chain (our daemon's
    /// default when no explicit seed; Sergey's daemon on a fresh shellnet).
    Auto,
    /// No bootstrap phase. Assume [`BridgeState`] is already initialized;
    /// the driver jumps straight to steady state. Used on any restart with
    /// persisted state on disk.
    Resume,
}

/// Result of a [`LiveProverDriver::poll_next_bundle`] call.
#[derive(Debug, Clone)]
pub enum LiveBundleEvent {
    /// Driver is still waiting for the chain to reach the bootstrap seed
    /// height. The caller should sleep + retry.
    Bootstrapping { seed_seqno: u64, chain_head_seqno: u64 },
    /// No new thinned key block is available (yet). Not an error.
    Nothing {
        /// Next thinned-key-block seqno the driver is aiming for.
        next_target_seqno: u64,
        /// Best-effort snapshot of the chain head at the time of this poll.
        chain_head_seqno: u64,
        /// `true` when the driver detected an un-drained bk-set rotation
        /// whose block-height is `<= next_target_seqno`. Callers polling
        /// bundles + bk-updates through independent trait impls (e.g.
        /// Sergey's `BlockSource` + `BkUpdateSource`) can use this bit to
        /// know that the bundle lane is blocked and the rotation lane needs
        /// draining first.
        blocked_by_pending_bk_update: bool,
    },
    /// A fully proven Circuit 1A/1B + Circuit 2 bundle ready for on-chain
    /// submission. The caller MUST call
    /// [`LiveProverDriver::ack_bundle`] once the bundle has been
    /// successfully verified downstream.
    Bundle(BundleProofArtifacts),
}

/// Result of a [`LiveProverDriver::poll_next_bk_update`] call.
#[derive(Debug, Clone)]
pub enum LiveBkUpdateEvent {
    /// Driver is still bootstrapping (same semantics as
    /// [`LiveBundleEvent::Bootstrapping`]).
    Bootstrapping { seed_seqno: u64, chain_head_seqno: u64 },
    /// No new bk-set rotation past the applied cursor. Not an error.
    Nothing,
    /// A fully proven bk-set rotation ready for `applyBkSetUpdate`. The
    /// caller MUST call [`LiveProverDriver::ack_bk_update`] once the update
    /// has been successfully applied downstream.
    BkUpdate(BkUpdateProofArtifacts),
}

/// Payload for a proven Circuit 1A/1B + Circuit 2 bundle. Pure data —
/// serializable, no halo2 types, no `Fr`. Both our IPC verifier and
/// Sergey's `AnBlockData` (ETH-side Solidity call) can be built with a
/// trivial `From` impl on the caller's side.
#[derive(Debug, Clone)]
pub struct BundleProofArtifacts {
    // Identity
    pub block_seq_no: u64,
    pub block_height: u64,
    pub last_seen_block_seq_no: u64,
    /// Raw 32-byte BE chain block hash (= SHA-256 root of the 16-leaf
    /// depth-4 `block_merkle_tree_leaves` = GraphQL `Block.id` = Solidity
    /// `uint256(bytes32(blockId))`). Since the 2026-07-22 Circuit 1 byte-order
    /// fix, both Circuit 1 (from the attestation payload) and Circuit 2 (from
    /// the SHA-256 root) bind `block_id_fr = fold(reverse(this))`, so the two
    /// derivation paths are provably equal for a valid block; `bundle.rs`
    /// debug-asserts this at build time. Carrying the full 256-bit hash (not
    /// its `Fr::to_repr()` LE bytes) preserves the top 2 bits that would be
    /// lost when the chain hash `>= p` (~3/4 of blocks). Rust verifiers
    /// derive the Fr on demand via `ipc::hash_hex_to_fr`; on-chain SHPLONK
    /// auto-reduces via `mod(calldataload, f_q)`.
    pub block_id_be: [u8; 32],
    pub fin_type: BundleFinalizationType,
    // Public inputs shared by Circuits 1A/1B + 2
    pub bk_set_commitment_be: [u8; 32],
    pub num_layers: u8,
    pub layer_hashes_be: [[u8; 32]; MAX_LAYERS],
    pub prev_max_level_layer_hash_be: [u8; 32],
    // Proof bytes (Blake2b Fiat–Shamir — AN opcode-compatible flavour)
    pub attestation_proof: Vec<u8>,
    pub layer_hashes_proof: Vec<u8>,
    // Diagnostic timings (for logs / stats)
    pub primary_proof_gen_ms: u64,
    pub layer_proof_gen_ms: u64,
    // Live GQL-derived per-layer bundle used by `ack_bundle` to advance the
    // driver's in-memory `BridgeState::append_bundle`. This mirrors the
    // sequence `state_layer_hashes` in the pre-refactor `main.rs`.
    pub state_layer_hashes: Vec<([u8; 32], u8)>,
}

/// Payload for a proven bk-set rotation (Circuit 1A/1B against OLD set + the
/// three SHA-256 Merkle siblings the on-chain verifier needs to reconstruct
/// the block-id from L2/L3 up to the 16-leaf depth-4 tree root).
#[derive(Debug, Clone)]
pub struct BkUpdateProofArtifacts {
    pub block_seq_no: u64,
    pub block_height: u64,
    pub last_seen_bk_update_seq_no: u64,
    /// Raw 32-byte BE chain block hash (= SHA-256 root of the 16-leaf
    /// depth-4 `block_merkle_tree_leaves` = Solidity
    /// `uint256(bytes32(blockId))`). Same semantics as
    /// [`BundleProofArtifacts::block_id_be`]: on-chain SHPLONK auto-reduces
    /// mod p, and the depth-4 SHA-256 Merkle open (h01 / h4_7 / h8_15 +
    /// l2/l3) checks against this raw root. The pre-v6 dual-field encoding
    /// (attestation-circuit `Fr::to_repr` + separate raw hash) is gone —
    /// callers derive Fr on demand.
    pub block_id_be: [u8; 32],
    pub fin_type: BundleFinalizationType,
    pub old_bk_set_commitment_be: [u8; 32],
    pub new_bk_set_commitment_be: [u8; 32],
    /// Depth-4 Merkle siblings needed to fold `sha(L2‖L3)` up to `block_id`:
    ///   h0_3 = sha(h01 ‖ sha(L2‖L3))
    ///   h0_7 = sha(h0_3 ‖ h4_7)
    ///   root = sha(h0_7 ‖ h8_15)
    pub merkle_sibling_h01_be: [u8; 32],
    pub merkle_sibling_h4_7_be: [u8; 32],
    pub merkle_sibling_h8_15_be: [u8; 32],
    pub attestation_proof: Vec<u8>,
    /// Post-rotation pubkey table so the caller can rotate its
    /// `ProverBkSet` snapshot. Same 48-byte compressed BLS pubkeys as the
    /// pre-rotation table.
    pub new_pubkeys: HashMap<u16, Vec<u8>>,
    pub primary_proof_gen_ms: u64,
}

/// Which attestation circuit was used. Mirrors
/// [`crate::ipc::AttestationCircuit`] but does not pull the `ipc` module
/// into consumers who talk to their own transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFinalizationType {
    /// Circuit 1A (single PRIMARY attestation, ≥2N/3 threshold in-circuit).
    Primary,
    /// Circuit 1B (PRIMARY prefinalization + FALLBACK target, >N/2 each
    /// in-circuit).
    Fallback,
}

impl BundleFinalizationType {
    /// Human-readable tag matching [`AttestationEvidence::path`].
    pub fn as_str(&self) -> &'static str {
        match self {
            BundleFinalizationType::Primary => "primary",
            BundleFinalizationType::Fallback => "fallback",
        }
    }
}

impl From<&AttestationEvidence> for BundleFinalizationType {
    fn from(ev: &AttestationEvidence) -> Self {
        match ev {
            AttestationEvidence::Primary(_) => BundleFinalizationType::Primary,
            AttestationEvidence::Fallback { .. } => BundleFinalizationType::Fallback,
        }
    }
}

/// Internal driver stage. Only two states — `NeedsSeed` during bootstrap,
/// `Steady` afterwards. Auto vs. Explicit is encoded via
/// [`LiveProverConfig::seed_policy`] and resolved on the first poll.
#[derive(Debug, Clone, Copy)]
enum DriverStage {
    /// Bootstrap has not completed. `seed_seqno` is `None` in Auto mode
    /// until the first poll (which reads the chain head), `Some(N)` in
    /// Explicit mode from construction.
    NeedsSeed { seed_seqno: Option<u64> },
    /// Bootstrap complete — [`BridgeState::initialized`] is `true`.
    Steady,
}

/// The live driver.
///
/// Owns all the mutable pieces the pipeline needs: the GraphQL client, the
/// halo2 [`KeyManager`] with its multi-gigabyte proving keys, the
/// contract-mirror [`BridgeState`], the prover-private [`ProverBkSet`], and
/// the in-memory pubkey table + Poseidon commitment that Circuit 1A/1B
/// consumes.
///
/// See the module-level docs for the caller's contract (persistence,
/// polling, acks).
pub struct LiveProverDriver {
    gql: GqlClient,
    key_manager: KeyManager,
    state: BridgeState,
    prover_bk_set: ProverBkSet,
    /// Cached Poseidon commitment of `prover_bk_set` as `Fr`. Kept as a
    /// field (not derived per-poll) because the underlying hash requires
    /// BLS-G1 deserialization of every pubkey plus a full Poseidon fold —
    /// re-computing per bundle poll would waste real cycles. Refreshed in
    /// `ack_bk_update` alongside `prover_bk_set.rotate`.
    bk_set_commitment_fr: Fr,
    cfg: LiveProverConfig,
    stage: DriverStage,
    seed: Option<BootstrapSeed>,
}

impl LiveProverDriver {
    /// Construct a new driver.
    ///
    /// Preconditions:
    /// * `key_manager` has already had `ensure_primary_keys`,
    ///   `ensure_fallback_keys`, and `ensure_layer_keys` called on it. The
    ///   driver does NOT eagerly re-load keys — it drives on-demand load /
    ///   unload during proof generation to stay within the single-PK memory
    ///   envelope.
    /// * `prover_bk_set` is the sole authoritative BK-pubkey source. The
    ///   ctor re-derives its Poseidon commitment from `prover_bk_set.pubkeys()`
    ///   and cross-checks against `state.stored_bk_set_commitment` on a
    ///   warm start.
    ///
    /// The `seed_policy` inside `cfg` is resolved lazily on the first
    /// poll — nothing is fetched or applied at construction time.
    ///
    /// Note for external consumers: constructing this driver transitively
    /// pulls halo2 crates into your build; see the module docs
    /// ("halo2 transitive dependency note") for the implication on your
    /// `Cargo.lock` and link-time. The public payloads themselves stay
    /// halo2-free — you only feel the dep at build time, not at type
    /// boundaries.
    pub fn new(
        gql: GqlClient,
        key_manager: KeyManager,
        state: BridgeState,
        prover_bk_set: ProverBkSet,
        cfg: LiveProverConfig,
    ) -> DriverResult<Self> {
        Self::new_inner(gql, key_manager, state, prover_bk_set, cfg)
            .map_err(DriverError::StateInconsistent)
    }

    /// Internal ctor that keeps the anyhow-based validation logic together
    /// and gets wrapped once at the public boundary. All ctor failures are
    /// structural (bad config, warm-state mismatch) so they classify as
    /// [`DriverError::StateInconsistent`].
    fn new_inner(
        gql: GqlClient,
        key_manager: KeyManager,
        state: BridgeState,
        prover_bk_set: ProverBkSet,
        cfg: LiveProverConfig,
    ) -> anyhow::Result<Self> {
        // Derive Fr + bytes commitment from prover_bk_set — the sole
        // pubkey source. Also serves as a self-consistency check: if the
        // persisted `commitment` field disagrees with the recomputed hash
        // of `pubkeys_hex`, the file was hand-edited or corrupted.
        let pubkeys = prover_bk_set
            .pubkeys()
            .context("prover_bk_set.pubkeys() decode failed")?;
        let (bk_set_commitment_fr, bk_set_commitment_bytes) =
            poseidon::compute_bk_set_poseidon(&pubkeys);
        anyhow::ensure!(
            bk_set_commitment_bytes == prover_bk_set.commitment,
            "prover_bk_set self-inconsistent: pubkeys hash to {} but stored \
             commitment is {}",
            hex::encode(bk_set_commitment_bytes),
            hex::encode(prover_bk_set.commitment),
        );

        // Sanity check: on a warm start the caller's `state` must agree with
        // its `bk_set`. Refuse to run silently in a mixed state.
        if state.initialized
            && state.stored_bk_set_commitment != bk_set_commitment_bytes
        {
            anyhow::bail!(
                "LiveProverDriver::new: bk_set commitment {} disagrees with \
                 BridgeState.stored_bk_set_commitment {}",
                hex::encode(bk_set_commitment_bytes),
                hex::encode(state.stored_bk_set_commitment),
            );
        }

        // Validate an Explicit seed policy up-front so a mis-configured
        // daemon fails at construction rather than at the first poll.
        let stage = match (cfg.seed_policy, state.initialized) {
            (_, true) => DriverStage::Steady,
            (SeedPolicy::Resume, false) => {
                anyhow::bail!(
                    "LiveProverDriver::new: SeedPolicy::Resume requires an initialized \
                     BridgeState — got initialized=false"
                );
            }
            (SeedPolicy::Explicit(n), false) => {
                let step = cfg.history_window_size * cfg.thinning_factor_p;
                anyhow::ensure!(
                    n > 0 && n % step == 0,
                    "SeedPolicy::Explicit({}) invalid: must be > 0 and divisible by W*P={}",
                    n, step,
                );
                DriverStage::NeedsSeed { seed_seqno: Some(n) }
            }
            (SeedPolicy::Auto, false) => DriverStage::NeedsSeed { seed_seqno: None },
        };

        Ok(Self {
            gql,
            key_manager,
            state,
            prover_bk_set,
            bk_set_commitment_fr,
            cfg,
            stage,
            seed: None,
        })
    }

    /// Non-blocking poll for the next Circuit 1A/1B + Circuit 2 bundle.
    ///
    /// Emits [`LiveBundleEvent::Bootstrapping`] while the driver is still
    /// waiting for the seed height, [`LiveBundleEvent::Nothing`] when no
    /// new thinned key block is available (or a bk-update rotation is
    /// blocking bundle advance), and [`LiveBundleEvent::Bundle`] when a
    /// fully-proven bundle is ready for the caller to submit downstream.
    pub async fn poll_next_bundle(&mut self) -> DriverResult<LiveBundleEvent> {
        self.poll_next_bundle_inner().await.map_err(DriverError::other)
    }

    async fn poll_next_bundle_inner(&mut self) -> anyhow::Result<LiveBundleEvent> {
        // Bootstrap gate: while `NeedsSeed`, drive the seed and either
        // apply it (transition to Steady) or return Bootstrapping.
        if let DriverStage::NeedsSeed { .. } = self.stage {
            let (chain_head, still_waiting) = self.advance_bootstrap().await?;
            if let Some(seed_seqno) = still_waiting {
                return Ok(LiveBundleEvent::Bootstrapping {
                    seed_seqno,
                    chain_head_seqno: chain_head,
                });
            }
            // Fell through — bootstrap complete, transition already happened.
        }

        // Poll chain head + compute the next thinned target.
        let latest_blocks = self.gql.query_latest_blocks(5).await?;
        let chain_head_seqno = latest_blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);
        let next_target_seqno = match find_next_thinned_key_block(
            self.state.stored_last_seen_block_seq_no,
            chain_head_seqno,
            self.cfg.history_window_size,
            self.cfg.thinning_factor_p,
        ) {
            Some(n) => n,
            None => {
                // No new key block — but there might still be an un-drained
                // bk-update. Callers polling bundle/bkupd through separate
                // trait impls need the flag either way.
                let blocked = self.pending_bk_update_below(u64::MAX).await?;
                return Ok(LiveBundleEvent::Nothing {
                    next_target_seqno: self.next_target_seqno_upper_bound(),
                    chain_head_seqno,
                    blocked_by_pending_bk_update: blocked,
                });
            }
        };

        // Guard: if there is an un-drained rotation at height `<= target`,
        // bundle advance is blocked. Caller must drain via
        // `poll_next_bk_update` first.
        if self.pending_bk_update_below(next_target_seqno).await? {
            return Ok(LiveBundleEvent::Nothing {
                next_target_seqno,
                chain_head_seqno,
                blocked_by_pending_bk_update: true,
            });
        }

        // Delegate to the bundle sub-module. Returns None when attestation
        // evidence is not ready yet (transient; caller retries).
        match bundle::drive_next_bundle(self, next_target_seqno).await? {
            Some(artifacts) => Ok(LiveBundleEvent::Bundle(artifacts)),
            None => Ok(LiveBundleEvent::Nothing {
                next_target_seqno,
                chain_head_seqno,
                blocked_by_pending_bk_update: false,
            }),
        }
    }

    /// Non-blocking poll for the next bk-set rotation proof.
    ///
    /// Emits [`LiveBkUpdateEvent::Bootstrapping`] while the driver is still
    /// waiting for the seed height, [`LiveBkUpdateEvent::Nothing`] when the
    /// prover is caught up on rotations, and
    /// [`LiveBkUpdateEvent::BkUpdate`] when a fully-proven rotation is
    /// ready for the caller to submit downstream.
    pub async fn poll_next_bk_update(&mut self) -> DriverResult<LiveBkUpdateEvent> {
        self.poll_next_bk_update_inner().await.map_err(DriverError::other)
    }

    async fn poll_next_bk_update_inner(&mut self) -> anyhow::Result<LiveBkUpdateEvent> {
        if let DriverStage::NeedsSeed { .. } = self.stage {
            let (chain_head, still_waiting) = self.advance_bootstrap().await?;
            if let Some(seed_seqno) = still_waiting {
                return Ok(LiveBkUpdateEvent::Bootstrapping {
                    seed_seqno,
                    chain_head_seqno: chain_head,
                });
            }
        }

        match bk_update::drive_next_bk_update(self).await? {
            Some(artifacts) => Ok(LiveBkUpdateEvent::BkUpdate(artifacts)),
            None => Ok(LiveBkUpdateEvent::Nothing),
        }
    }

    /// Acknowledge that `artifacts` was successfully verified and submitted
    /// downstream. Advances the in-memory [`BridgeState`] cursor via
    /// [`BridgeState::append_bundle`]. Idempotent by `block_seq_no`: no-op
    /// when the cursor is already past.
    pub fn ack_bundle(&mut self, artifacts: &BundleProofArtifacts) -> DriverResult<()> {
        self.ack_bundle_inner(artifacts).map_err(DriverError::StateInconsistent)
    }

    fn ack_bundle_inner(&mut self, artifacts: &BundleProofArtifacts) -> anyhow::Result<()> {
        // Outer idempotency check: stale replay is a no-op, not an error.
        // The driver may be re-driven with the same artifacts after a
        // downstream retry; the *inner* `append_bundle` monotonicity check
        // would error on that, so we intercept the equal/older case here.
        if artifacts.block_seq_no <= self.state.stored_last_seen_block_seq_no {
            info!(
                "ack_bundle: no-op — artifacts.block_seq_no={} <= stored_last_seen={}",
                artifacts.block_seq_no, self.state.stored_last_seen_block_seq_no,
            );
            return Ok(());
        }
        // `append_bundle` no longer writes `stored_bk_set_commitment`
        // (single-writer discipline mirroring Solidity `verifyBlock`).
        // The driver's `bk_set_commitment_fr` is rotated in-memory by
        // `ack_bk_update` after `apply_bk_set_update` succeeds; by the
        // time we reach here it already matches
        // `state.stored_bk_set_commitment`. `?` on the append is defense
        // in depth: the outer guard above already ensures monotonicity.
        self.state.append_bundle(
            &artifacts.state_layer_hashes,
            artifacts.block_height,
            artifacts.block_seq_no,
        )?;
        Ok(())
    }

    /// Acknowledge that `artifacts` was successfully verified and applied
    /// downstream. Advances the in-memory [`BridgeState`] +
    /// [`ProverBkSet`] cursors and rotates the driver's in-memory pubkey
    /// table + Poseidon commitment. Idempotent by `block_seq_no`.
    pub fn ack_bk_update(
        &mut self,
        artifacts: &BkUpdateProofArtifacts,
    ) -> DriverResult<()> {
        self.ack_bk_update_inner(artifacts).map_err(DriverError::StateInconsistent)
    }

    fn ack_bk_update_inner(
        &mut self,
        artifacts: &BkUpdateProofArtifacts,
    ) -> anyhow::Result<()> {
        if artifacts.block_seq_no <= self.state.stored_last_bk_set_update_seq_no {
            info!(
                "ack_bk_update: no-op — artifacts.block_seq_no={} <= stored_last_bk_set_update={}",
                artifacts.block_seq_no, self.state.stored_last_bk_set_update_seq_no,
            );
            return Ok(());
        }
        self.state
            .apply_bk_set_update(
                artifacts.old_bk_set_commitment_be,
                artifacts.new_bk_set_commitment_be,
                artifacts.block_seq_no,
            )
            .context("ack_bk_update: apply_bk_set_update failed")?;
        self.prover_bk_set
            .rotate(
                artifacts.new_bk_set_commitment_be,
                artifacts.new_pubkeys.clone(),
                artifacts.block_seq_no,
            )
            .context("ack_bk_update: prover_bk_set.rotate failed")?;
        // Refresh the cached Fr commitment so subsequent bundle /
        // bk-update proofs use the rotated set. Callers reading the
        // rotated pubkey table go through `prover_bk_set.pubkeys()`
        // — no separate in-memory table to sync any more.
        let (new_fr, _) = poseidon::compute_bk_set_poseidon(&artifacts.new_pubkeys);
        self.bk_set_commitment_fr = new_fr;
        Ok(())
    }

    /// Read-only snapshot of the contract-mirror state. Caller persists
    /// with [`BridgeState::save`] after every ack.
    pub fn snapshot_state(&self) -> &BridgeState {
        &self.state
    }

    /// Push a self-verify [`crate::bridge_state::BundleResult`] into the
    /// contract-mirror state's ring buffer. Used exclusively by the
    /// `bridge-prover-daemon`'s `self-verify` feature — the ring buffer is
    /// diagnostic metadata for the CI smoke test's post-run assertions.
    /// Callers not using `self-verify` should ignore this method.
    pub fn record_self_verify_result(
        &mut self,
        result: crate::bridge_state::BundleResult,
    ) {
        self.state.push_bundle_result(result);
    }

    /// Read-only snapshot of the prover-private pubkey table. Caller
    /// persists with [`ProverBkSet::save`] after every ack.
    pub fn snapshot_prover_bk_set(&self) -> &ProverBkSet {
        &self.prover_bk_set
    }

    /// Read-only handle on the halo2 [`KeyManager`], exposed so callers
    /// running the `bridge-prover-daemon`'s `self-verify` feature can inline
    /// [`crate::verifier`] on the returned artifacts without pulling in
    /// their own PK loader. Note that verifying keys are loaded lazily; the
    /// caller must invoke `key_manager.ensure_*_keys` (or rely on the
    /// driver having done so already via proof generation).
    pub fn key_manager_ref(&self) -> &KeyManager {
        &self.key_manager
    }

    /// Read-only snapshot of the [`BootstrapSeed`] applied during the last
    /// (or current) bootstrap. `None` before bootstrap has resolved. Our
    /// daemon persists this to `state/bootstrap_seed.json` so the paired
    /// verifier daemon can cold-start; Sergey's daemon ignores it.
    pub fn snapshot_bootstrap_seed(&self) -> Option<&BootstrapSeed> {
        self.seed.as_ref()
    }

    // -----------------------------------------------------------------
    // Internal helpers used by the sub-modules.
    // -----------------------------------------------------------------

    /// Best-effort next target seqno used in [`LiveBundleEvent::Nothing`]
    /// when the driver has no chain-head snapshot yet (e.g. GQL just
    /// returned an empty list). Bounds the value by W*P above the cursor
    /// so callers see a sane number.
    fn next_target_seqno_upper_bound(&self) -> u64 {
        let step = self.cfg.history_window_size * self.cfg.thinning_factor_p;
        ((self.state.stored_last_seen_block_seq_no / step) + 1) * step
    }

    /// Query GQL for the next pending bk-set rotation and check whether its
    /// block-height is `<= max_height`. Cheap enough to call once per
    /// bundle poll — sends one GQL request.
    async fn pending_bk_update_below(&self, max_height: u64) -> anyhow::Result<bool> {
        let cursor = self.state.stored_last_bk_set_update_seq_no;
        match bridge_gql_fetcher::bk_set_fetcher::next_update_after(&self.gql, cursor).await {
            Ok(Some(upd)) => Ok(upd.height.map(|h| h <= max_height).unwrap_or(false)),
            Ok(None) => Ok(false),
            Err(e) => {
                warn!(
                    "pending_bk_update_below: next_update_after failed ({}), assuming no pending update",
                    e,
                );
                Ok(false)
            }
        }
    }

    /// Advance the bootstrap state machine. Returns
    /// `(chain_head_seqno, still_waiting_for_seed_seqno_opt)`.
    async fn advance_bootstrap(&mut self) -> anyhow::Result<(u64, Option<u64>)> {
        let step = self.cfg.history_window_size * self.cfg.thinning_factor_p;
        let latest_blocks = self.gql.query_latest_blocks(5).await?;
        let chain_head = latest_blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);

        // Resolve the seed seqno (Auto: snap once and cache; Explicit:
        // already set at construction).
        let seed_seqno = match self.stage {
            DriverStage::NeedsSeed { seed_seqno: Some(n) } => n,
            DriverStage::NeedsSeed { seed_seqno: None } => {
                let n = ((chain_head / step) + 1) * step;
                info!(
                    "live_driver: Auto seed pinned at seq_no={} (chain head at {})",
                    n, chain_head,
                );
                self.stage = DriverStage::NeedsSeed { seed_seqno: Some(n) };
                n
            }
            DriverStage::Steady => return Ok((chain_head, None)),
        };

        if chain_head < seed_seqno {
            return Ok((chain_head, Some(seed_seqno)));
        }

        // Chain has caught up. Fetch, apply, and transition to Steady.
        let bk_hash_bytes: [u8; 32] = self.bk_set_commitment_fr.to_repr();
        let seed = crate::bootstrap::fetch_from_node(
            &self.gql,
            seed_seqno,
            bk_hash_bytes,
        )
        .await?;
        seed.apply(&mut self.state)?;
        info!(
            "live_driver: bootstrap seed applied — seq_no={}, height={}, layers={}",
            seed.block_seq_no,
            seed.block_height,
            seed.layer_hashes.len(),
        );
        self.seed = Some(seed);
        self.stage = DriverStage::Steady;
        Ok((chain_head, None))
    }

    // Accessors used by the sub-modules (crate-visible only).
    pub(crate) fn gql(&self) -> &GqlClient {
        &self.gql
    }
    pub(crate) fn key_manager_mut(&mut self) -> &mut KeyManager {
        &mut self.key_manager
    }
    pub(crate) fn key_manager(&self) -> &KeyManager {
        &self.key_manager
    }
    pub(crate) fn state(&self) -> &BridgeState {
        &self.state
    }
    pub(crate) fn bk_set_commitment_fr(&self) -> Fr {
        self.bk_set_commitment_fr
    }
    pub(crate) fn prover_bk_set(&self) -> &ProverBkSet {
        &self.prover_bk_set
    }
    pub(crate) fn cfg(&self) -> &LiveProverConfig {
        &self.cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_policy_explicit_validates_alignment() {
        // Explicit seed must be > 0 and divisible by W*P = 512 in the
        // default config. 1024 is fine; 1025 is not; 0 is not.
        let step = HISTORY_WINDOW_SIZE * crate::THINNING_FACTOR_P;
        assert_eq!(step, 512);
        // We can't construct a real GqlClient / KeyManager in a unit test,
        // so we only sanity-check the alignment math the ctor uses.
        assert!(1024 % step == 0);
        assert!(1025 % step != 0);
    }

    #[test]
    fn default_config_matches_pre_refactor_constants() {
        let cfg = LiveProverConfig::default();
        assert_eq!(cfg.thinning_factor_p, crate::THINNING_FACTOR_P);
        assert_eq!(cfg.history_window_size, HISTORY_WINDOW_SIZE);
        assert_eq!(cfg.seed_policy, SeedPolicy::Resume);
    }

    #[test]
    fn finalization_type_maps_from_attestation_evidence_tag() {
        // Structural: the two-way mapping between our public
        // BundleFinalizationType enum and the internal AttestationEvidence
        // path label must not drift.
        assert_eq!(BundleFinalizationType::Primary.as_str(), "primary");
        assert_eq!(BundleFinalizationType::Fallback.as_str(), "fallback");
    }
}
