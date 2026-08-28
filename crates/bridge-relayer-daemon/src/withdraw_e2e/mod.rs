//! In-process withdraw-E2E orchestrator (Circuit 4).
//!
//! This module replaces steps 5–7 of the Python driver
//! (`python/helper/bridge_e2e.py`) with a single Rust entrypoint. Given
//! a live GraphQL endpoint plus a loaded on-disk
//! [`bridge_prover_lib::bridge_state::BridgeState`] snapshot (the
//! `prover_state.json` written by the live prover driver), [`run_once`]:
//!
//! 1. captures the next `WithdrawalInitiated` ExtOut event via
//!    [`capture::capture_next_withdrawal_event`],
//! 2. builds a hermetic partial witness via
//!    [`bridge_event_witness::export_from_event_boc_base64`],
//! 3. enriches it via [`bridge_event_witness::enrich_witness`]
//!    (events-tree + block-tree Merkle proofs + anchor),
//! 4. runs the C4 SHPLONK pipeline
//!    ([`crate::aggregator::Circuit4ShplonkPipeline`]) —
//!    [`crate::aggregator::InProcessCircuit4SnarkProver`] re-proves the
//!    witness with a Poseidon transcript at K=19, then
//!    [`crate::aggregator::SubprocessAggregator`] shells out to
//!    `aggregate-proof --name BridgeWithdrawalAggregatorVerifier` to
//!    produce the 22-instance SHPLONK calldata the deployed Yul verifier
//!    accepts byte-for-byte.
//!
//! `run_once` intentionally does **not** touch Ethereum — it returns the
//! [`crate::withdrawal::PartnerWithdrawalProof`] and leaves ETH-side
//! submission to the caller (CLI subcommand or embedding loop). That
//! separation keeps this module free of wallet/provider assumptions and
//! mirrors the split the Python driver draws between "produce the proof
//! bytes" and "hand them to the on-chain verifier".

pub mod capture;
pub mod driver;

pub use capture::{
    capture_next_withdrawal_event, snapshot_baseline_msg_ids, CapturedEvent,
};
pub use driver::{run_once, run_once_with_state, WithdrawE2EConfig, WithdrawE2ESummary};
