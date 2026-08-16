//! Relayer skeleton for the Acki Nacki → Ethereum bridge.
//!
//! Live path (Alina 2026-07-09 plan): [`LiveBlockSource`] wraps
//! `bridge_prover_lib::live_driver::LiveProverDriver` and feeds
//! [`Relayer::tick`]'s two-phase loop (BK-update drain → verifyBlock).
//! File-driven sources ([`ProverProofsBlockSource`], [`FixturesBlockSource`])
//! remain for CI / operator one-shots.

pub mod aggregated_source;
pub mod aggregator;
pub mod bridge;
pub mod daemon;
pub mod error;
pub mod history_consistency;
pub mod live_source;
pub mod proof_validation;
pub mod relayer;
pub mod source;
pub mod state;
pub mod types;
pub mod withdraw_prover;
pub mod withdrawal;

pub use aggregated_source::AggregatedBlockSource;
pub use aggregator::{
    calldata_binds_instances, Circuit12ShplonkPipeline, Circuit4ShplonkPipeline,
    Circuit4SnarkProver, InProcessCircuit4SnarkProver, MockAggregator, MockCircuit4SnarkProver,
    MockSnarkWrapper, PoseidonSnarkWrapper, ProofAggregator, SnarkArtefacts, SnarkWrapper,
    SubprocessAggregator, SubprocessAggregatorConfig, FALLBACK_VERIFIER_NAME,
    LAYER_HASHES_VERIFIER_NAME, PRIMARY_VERIFIER_NAME, WITHDRAWAL_VERIFIER_NAME,
};
pub use bridge::{
    BkSetUpdateSubmitOutcome, BridgeClient, BridgeOnChainState, DryRunOutcome, EthBridgeClient,
    MockBridgeClient, SubmitOutcome, WithdrawSubmitOutcome,
};
pub use daemon::{
    BackoffConfig, DaemonRunSummary, LastOutcome, RelayerMetrics, RelayerMetricsSnapshot,
};
pub use error::RelayerError;
pub use history_consistency::{
    check_chain_monotonicity, check_history_consistency, check_startup_drift, HistoryDrift,
};
pub use live_source::{LiveBlockSource, StatePaths};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{
    BkUpdateProofsSource, BkUpdateSource, BlockSource, EmptyBkUpdateSource, FixturesBlockSource,
    InMemoryBlockSource, ProverProofsBlockSource,
};
pub use state::RelayerState;
pub use types::{AnBlockData, BkSetUpdateData, FinalizationType, MAX_LAYER_HASHES};
pub use withdraw_prover::{
    MockWithdrawalProver, SubprocessWithdrawalProver, SubprocessWithdrawalProverConfig,
    WithdrawalProver, PROVER_BIN,
};
pub use withdrawal::{
    discover_event_proofs, is_event_proof_file, PartnerWithdrawalProof, WithdrawalPublicInputs,
};
