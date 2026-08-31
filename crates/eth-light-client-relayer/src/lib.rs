//! Relayer for the Ethereum beacon **light-client** oracle on Acki Nacki.
//!
//! Polls `GET /eth/v1/beacon/light_client/finality_update`, proves a step
//! (subprocess into `eth-light-client-prover`, or a mock), and calls
//! `EthBeaconLightClient.submitUpdate`. Committee rotation is detected as a
//! period jump; auto-rotate is **off** by default (n14 + tvm-sdk#284).
//!
//! Does **not** flip `USDCBridge.finalizeDeposit` onto this oracle — attesters
//! stay the canonicality writer until ancestry + live E2E.

pub mod an_config;
pub mod daemon;
pub mod error;
pub mod prover;
pub mod relayer;
pub mod source;
pub mod state;
pub mod submitter;
pub mod types;

pub use an_config::AnConfig;
pub use daemon::{BackoffConfig, RelayerMetrics};
pub use error::RelayerError;
pub use prover::{
    MockProofGenerator, ProofGenerator, SubprocessProofGenerator, SubprocessProverConfig,
};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{BeaconSource, HttpBeaconSource, InMemoryBeaconSource};
pub use state::{RelayerState, StateLock};
pub use submitter::{
    build_submit_params, AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, MockAnSubmitter,
    SubmitOutcome,
};
pub use types::{
    pack_step_public_inputs, parse_finality_update, FinalityUpdate, RotateProofBundle,
    StepProofBundle, SLOTS_PER_SYNC_PERIOD, STEP_INSTANCE_LEN,
};
