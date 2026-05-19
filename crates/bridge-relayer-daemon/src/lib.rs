//! Relayer skeleton for the Acki Nacki → Ethereum bridge.
//!
//! ## Phase scope (5.1)
//!
//! - Defines the **`BlockSource`** abstraction that decouples the relayer loop
//!   from any specific AN node integration. The future [`LiveBlockSource`]
//!   (Phase 5.2) will be a single implementation backed by the partner's
//!   `gql_client` + `boc_parser` + our [`bridge-prover-orchestrator`].
//! - Defines the **`BridgeClient`** abstraction over the on-chain
//!   `AckiNackiBridge.verifyBlock` entry point. The real implementation in
//!   [`bridge::EthBridgeClient`] uses alloy-rs `sol!`-generated bindings; the
//!   in-memory mirror in [`bridge::MockBridgeClient`] is used by the unit tests
//!   in this crate to drive multi-block scenarios without spawning Anvil.
//! - Implements the **main loop** ([`Relayer::run_loop`]) and the single-step
//!   entry ([`Relayer::tick`]) with structured tracing, on-disk crash-recovery
//!   state ([`state::RelayerState`]), and a clean error taxonomy
//!   ([`error::RelayerError`]).
//! - Provides an [`InMemoryBlockSource`] for unit testing and a
//!   [`FixturesBlockSource`] that re-uses the Phase 4.1 bound proof artefacts
//!   as a one-shot canned block for smoke testing the live submission path
//!   against a local Anvil.
//!
//! ## Out of scope (handled by Phase 5.2 / 5.3)
//!
//! - GraphQL / BOC parsing of the live AN node.
//! - Halo2 + gnark wrapping invocation from inside the relayer (Phase 5.2 wires
//!   the orchestrator + a Go FFI or subprocess).
//! - 10 sequential blocks against shellnet — that's the Phase 5 acceptance
//!   criterion from §5 of the integration plan.
//!
//! ## High-level flow
//!
//! ```text
//! ┌─────────────────────────┐    ┌──────────────────────────┐
//! │  BlockSource (trait)    │◀── │ Relayer::tick()          │
//! │  - LiveBlockSource (TBD)│    │  1. read on-chain anchor │
//! │  - FixturesBlockSource  │    │  2. fetch(target_seqno)  │
//! │  - InMemoryBlockSource  │    │  3. submit_block(...)    │
//! └─────────────────────────┘    │  4. persist state        │
//!                                └────────────┬─────────────┘
//!                                             │
//!                                             ▼
//!                                ┌──────────────────────────┐
//!                                │ BridgeClient (trait)     │
//!                                │ - EthBridgeClient        │
//!                                │ - MockBridgeClient (test)│
//!                                └──────────────────────────┘
//! ```

pub mod bk_set_sentry;
pub mod bridge;
pub mod daemon;
pub mod error;
pub mod guarded_relayer;
pub mod relayer;
pub mod source;
pub mod state;
pub mod types;

pub use bk_set_sentry::{BkSetPoller, BkSetSentry, SentryMetrics, SentryStatus};
pub use bridge::{
    BridgeClient, BridgeOnChainState, EthBridgeClient, MockBridgeClient, SubmitOutcome,
};
pub use daemon::{
    BackoffConfig, DaemonRunSummary, LastOutcome, RelayerMetrics, RelayerMetricsSnapshot,
};
pub use error::RelayerError;
pub use guarded_relayer::{GuardedOutcome, SentryGuardedRelayer};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{BlockSource, FixturesBlockSource, InMemoryBlockSource};
pub use state::RelayerState;
pub use types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES};
