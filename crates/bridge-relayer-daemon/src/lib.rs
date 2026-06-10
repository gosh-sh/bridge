//! Relayer skeleton for the Acki Nacki → Ethereum bridge.
//!
//! ## Phase scope (5.1 + 5.2 scaffolding)
//!
//! - Defines the **`BlockSource`** abstraction that decouples the relayer loop
//!   from any specific AN node integration. The [`LiveBlockSource`] composition
//!   (Phase 5.2 scaffolding) splits the live path into two narrow traits —
//!   [`RawBlockProvider`] (HTTP / GraphQL against the AN node) and
//!   [`BoundProofGenerator`] (Halo2 + gnark wrap) — so each side can be swapped
//!   independently. Stub backends (`InMemoryRawBlockProvider`,
//!   `StubBoundProofGenerator`) ship with the crate; real backends land in
//!   Phase 5.2 (`circuit-data-exporter` adapter) and Phase 6
//!   (`bridge-prover-daemon` IPC).
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
//! - Real `RawBlockProvider` backed by partner's GraphQL endpoint (gated on
//!   public exposure; today only `/v2/bk_set_update` REST is reachable on
//!   port 8600 — see `AGENTS.md`). A local-cluster path through
//!   `http://127.0.0.1:11000/graphql` is feasible once the cluster builds
//!   with the `history_proofs` feature.
//! - Real `BoundProofGenerator` invoking the orchestrator + gnark wrappers
//!   (Phase 6 `bridge-prover-daemon` over IPC; in-process Halo2 takes minutes
//!   per call and would starve the relayer's single-tick budget).
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
pub mod live_source;
pub mod relayer;
pub mod source;
pub mod state;
pub mod types;
pub mod withdrawal;

pub use bk_set_sentry::{BkSetPoller, BkSetSentry, SentryMetrics, SentryStatus};
pub use bridge::{
    BridgeClient, BridgeOnChainState, DryRunOutcome, EthBridgeClient, MockBridgeClient,
    SubmitOutcome, WithdrawSubmitOutcome,
};
pub use daemon::{
    BackoffConfig, DaemonRunSummary, LastOutcome, RelayerMetrics, RelayerMetricsSnapshot,
};
pub use error::RelayerError;
pub use guarded_relayer::{GuardedOutcome, SentryGuardedRelayer};
pub use live_source::{
    BoundProofArtifacts, BoundProofGenerator, InMemoryRawBlockProvider, LiveBlockSource,
    RawBlockProvider, RawBlockWitness, StubBoundProofGenerator,
};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{BlockSource, FixturesBlockSource, InMemoryBlockSource, ProverProofsBlockSource};
pub use state::RelayerState;
pub use types::{AnBlockData, FinalizationType, MAX_LAYER_HASHES};
pub use withdrawal::{PartnerWithdrawalProof, WithdrawalPublicInputs, GROTH16_PROOF_SIZE};
