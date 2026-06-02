//! Relayer for the **EVM→Acki Nacki deposit direction**.
//!
//! This is the mirror of `bridge-relayer-daemon` (which handles AN→ETH state
//! via `verifyBlock`). Here the relayer automates the *deposit* leg of the
//! bridge:
//!
//! 1. **Listen** for `AckiNackiBridge.Deposit(depositId, sender, amount,
//!    timestamp)` events on Ethereum, surfacing only deposits buried under a
//!    configurable confirmation depth ([`source::EthLogSource`]).
//! 2. **Prove** that the deposit event was emitted — the `deposit-prover`
//!    Halo2 circuit (K=18, RLC) produces a Blake2b-transcript SHPLONK proof
//!    plus the seven public inputs `[depositId, sender, amount,
//!    contractAddress, blockHashHigh, blockHashLow, promiseCommit]`. Because
//!    `deposit-prover` is its own cargo workspace, the proof is generated
//!    out-of-process ([`prover::SubprocessProofGenerator`]).
//! 3. **Submit** the resulting proof triple (`vk_blob`, `public_inputs`,
//!    `proof`) to the AN-side `TokenBridge.finalizeDeposit(...)`, which
//!    verifies it natively via the `ZKHALO2VERIFYWITHVK` opcode and consumes
//!    the `usedDepositIds` nullifier ([`submitter::AnInterfaceSubmitter`]).
//!
//! ## Architecture
//!
//! Three trait seams keep the loop testable and let the heavy / not-yet-built
//! pieces be swapped independently:
//!
//! ```text
//! ┌────────────────────────┐   ┌──────────────────────────┐   ┌────────────────────────┐
//! │ DepositSource          │──▶│ Relayer::tick()          │──▶│ AnSubmitter            │
//! │  - EthLogSource (alloy)│   │ 1. nullifier pre-check   │   │  - AnInterfaceSubmitter│
//! │  - InMemoryDepositSrc  │   │ 2. fetch deposit event   │   │  - MockAnSubmitter     │
//! └────────────────────────┘   │ 3. generate proof        │   └────────────────────────┘
//!            ▲                  │ 4. submit + finalise     │
//!            │                  │ 5. persist state         │
//! ┌────────────────────────┐   └────────────┬─────────────┘
//! │ ProofGenerator         │◀───────────────┘
//! │  - SubprocessProofGen  │   ┌──────────────────────────┐
//! │  - MockProofGenerator  │   │ state.json (depositId)   │
//! └────────────────────────┘   └──────────────────────────┘
//! ```
//!
//! ## Status / blockers
//!
//! - **Event listening** ([`source::EthLogSource`]) and **proof generation**
//!   ([`prover::SubprocessProofGenerator`]) are fully wired against live
//!   Ethereum + the `deposit-prover` examples.
//! - **AN delivery** ([`submitter::AnInterfaceSubmitter`]) is wired over the
//!   [`acki_nacki_interface::IAckiNacki`] trait, but the only implementation
//!   of that trait today is the mock — the AN team will ship the live
//!   `tvm-sdk`-backed client. The `finalizeDeposit` call body uses an interim
//!   encoding ([`submitter::encode_finalize_deposit`]) pending the canonical
//!   TVM message ABI. The AN-side nullifier makes re-submission safe in the
//!   interim.
//! - The AN-side `TokenBridge.finalizeDeposit` entry point itself is on the
//!   partner branch `poseidon_dex_with_verify` (see
//!   `docs/zkhalo2verifywithvk_reference.md`).

pub mod an_config;
pub mod daemon;
pub mod error;
pub mod prover;
pub mod relayer;
pub mod source;
pub mod state;
pub mod submitter;
pub mod types;

pub use an_config::{AnConfig, AnPreflight, DEFAULT_AN_NODE_URL, DEFAULT_LOCAL_AN_NODE_URL};
pub use daemon::{
    BackoffConfig, DaemonRunSummary, LastOutcome, RelayerMetrics, RelayerMetricsSnapshot,
};
pub use error::RelayerError;
pub use prover::{
    MockProofGenerator, ProofGenerator, SubprocessProofGenerator, SubprocessProverConfig,
};
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{AckiNackiBridge, DepositSource, EthLogSource, InMemoryDepositSource};
pub use state::RelayerState;
pub use submitter::{
    decode_finalize_deposit, encode_finalize_deposit, AnInterfaceSubmitter, AnSubmitConfig,
    AnSubmitter, MockAnSubmitter, SubmitOutcome,
};
pub use types::{
    DepositEvent, DepositProofBundle, DepositPublicInputs, NUM_PUBLIC_INPUTS, PUBLIC_INPUT_BYTES,
};
