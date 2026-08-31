//! Relayer for the Ethereum beacon **light-client** oracle on Acki Nacki.
//!
//! Polls `finality_update` + `light_client/updates`, proves a step (real
//! 512-committee via `COMMITTEE_JSON_PATH`), and calls
//! `EthBeaconLightClient.submitUpdate`. Period jump → `submitRotate` by
//! default (`--no-rotate` opts out). tvm-sdk#284 co-deploys with this contract.
//! Epoch ancestry: [`ancestry`]
//! (off-chain) + [`header_rlp`] + `submitAncestry` (on-chain writer).
//! `finalizeDeposit` flip: `scripts/ursus/flip_deposit_to_light_client.md`.

pub mod an_config;
pub mod ancestry;
pub mod daemon;
pub mod error;
pub mod header_rlp;
pub mod prover;
pub mod relayer;
pub mod source;
pub mod state;
pub mod submitter;
pub mod types;

pub use an_config::AnConfig;
pub use ancestry::{covers_deposit, ExecLink};
pub use daemon::{BackoffConfig, RelayerMetrics};
pub use error::RelayerError;
pub use prover::{
    MockProofGenerator, ProofGenerator, SubprocessProofGenerator, SubprocessProverConfig,
};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{BeaconSource, EthExecutionRpc, HttpBeaconSource, InMemoryBeaconSource};
pub use state::{RelayerState, StateLock};
pub use submitter::{
    build_submit_params, AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, MockAnSubmitter,
    SubmitOutcome,
};
pub use types::{
    pack_step_public_inputs, parse_finality_update, FinalityUpdate, RotateProofBundle,
    StepProofBundle, SLOTS_PER_SYNC_PERIOD, STEP_INSTANCE_LEN,
};
