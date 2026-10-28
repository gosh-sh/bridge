//! Relayer for the Ethereum beacon **light-client** oracle on Acki Nacki.
//!
//! Polls `finality_update` + `light_client/updates`, proves a step (real
//! 512-committee via `COMMITTEE_JSON_PATH`), and calls
//! `EthBeaconLightClient.submitUpdate`. Period jump → `submitRotate` by
//! default (`--no-rotate` opts out). tvm-sdk#284 co-deploys with this contract.
//! Epoch ancestry: [`ancestry`]
//! (off-chain) + [`header_rlp`] + `submitAncestry` (on-chain writer).
//! `finalizeDeposit` flip, only with `--flip-owner`. After the first
//! accepted update it checks the bridge's `getLightClient(l1ChainId)` for
//! the chain its own client reports against `AN_LIGHT_CLIENT`, then calls
//! `disableOwnerAnchorsOn(l1ChainId)` + `disableOwnerRotation`. A Gloas
//! pin of `u64::MAX` is a setting (`setGloasForkSlot`), not a flip gate.
//! With
//! `ETH_RPC_URL` each accepted tick then `rePushAnchor`s the checkpoint and
//! runs `link_headers` locally on the span back to the previous checkpoint.
//! On-chain `submitAncestry` is `--submit-ancestry` (default off).
//!
//! [`arbitrum`] gets the Arbitrum blocks that hold deposits accepted through
//! `ArbitrumBlockVerifier.proveBlock`, on top of the parent-chain blocks the
//! light client accepts (`arbitrum-anchors`).

pub mod an_config;
pub mod ancestry;
pub mod arbitrum;
pub mod contract_model;
pub mod daemon;
pub mod error;
pub mod header_rlp;
pub mod prover;
pub mod relayer;
pub mod source;
pub mod state;
pub mod submitter;
pub mod types;

pub use an_config::{validate_live_graphql_url, AnConfig};
pub use ancestry::{covers_deposit, ExecLink};
pub use daemon::{BackoffConfig, RelayerMetrics};
pub use error::RelayerError;
pub use prover::{
    MockProofGenerator, ProofGenerator, SubprocessProofGenerator, SubprocessProverConfig,
};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{
    BeaconSource, ChainSpec, EthExecutionRpc, ExecutionSource, HttpBeaconSource,
    InMemoryBeaconSource, InMemoryExecution,
};
pub use state::{RelayerState, StateLock};
pub use submitter::{
    build_submit_params, le_word_to_uint256_hex, AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter,
    MockAnSubmitter, SubmitOutcome,
};
pub use types::{
    gloas_is_scheduled, pack_step_public_inputs, parse_finality_update, BeaconChainParams,
    FinalityUpdate, RotateProofBundle, StepProofBundle, GLOAS_NOT_SCHEDULED, SLOTS_PER_SYNC_PERIOD,
    STEP_INSTANCE_LEN,
};
