//! On-disk idempotency state. One file per withdrawal keyed by a stable
//! digest of `(from, to, to_chain, amount)`.
//!
//! Purpose: refuse a second broadcast of the same withdrawal if a prior
//! run got as far as sending. Ekaterina's spec calls this out as a
//! "решить до реализации" item — the answer we shipped is:
//! - v1: refuse-duplicate + blunt `--allow-retry` override.
//! - v2: `--resume` picks up mid-pipeline with `replay_latest` semantics
//!   already present in the relayer driver.
//!
//! Storage layout: one JSON file per key under
//! `$BRIDGE_CONFIG_DIR/withdraw-state/<hex-digest>.json`. Written on
//! every stage transition so a crash leaves a resumable trail. Files
//! never contain key material, key paths, or ETH private keys — only
//! chain-observable identifiers.
//!
//! File acquisition uses `OpenOptions::create_new()` for the first write
//! (atomic "no prior record" check), then plain overwrite for updates.
//! This is not a distributed lock — two concurrent CLI invocations
//! against the same key on different hosts could still race — but for
//! the "single user re-running my broken script" case it does the job.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::args::{FromAddress, ToAddress, UsdcAmount};
use crate::errors::CliResult;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Preflight passed, record reserved, no broadcast yet.
    Reserved,
    /// AN burn broadcast; tx hash recorded.
    Burned,
    /// WithdrawalInitiated event captured.
    Captured,
    /// Circuit-4 proof produced (self-verified locally).
    Proved,
    /// `withdrawByProof` broadcast — waiting on receipt.
    Submitted,
    /// `withdrawByProof` receipt received, mined.
    Confirmed,
    /// Terminal failure at some stage; needs `--allow-retry` (v1) or
    /// `--resume` (v2) to re-attempt.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub status: Status,
    pub from_extended: String,
    pub to_hex: String,
    pub to_chain: u64,
    pub amount_micro: u128,
    pub reserved_at: String,
    pub an_tx_hash: Option<String>,
    pub withdrawal_msg_id: Option<String>,
    pub block_seq_no: Option<u64>,
    pub proof_json_path: Option<PathBuf>,
    pub eth_tx_hash: Option<String>,
}

/// Compute the deduplication key. Deliberately does NOT include the
/// prover-state path, the wall-clock, or any nonce — the spec's intent
/// is "same withdrawal = same identity" so re-running the same command
/// with the same args refuses.
pub fn key(from: &FromAddress, to: &ToAddress, amount: &UsdcAmount) -> String {
    let mut h = Sha256::new();
    h.update(from.extended().as_bytes());
    h.update(b"|");
    h.update(to.address.as_slice());
    h.update(b"|");
    h.update(to.chain_id.to_be_bytes());
    h.update(b"|");
    h.update(amount.0.to_be_bytes());
    hex::encode(h.finalize())
}

/// Try to reserve the record. Returns `Err(DuplicateInFlight)` if a prior
/// record exists AND its status is not `Failed`. On `Ok`, the returned
/// [`Record`] is already persisted to disk with `Status::Reserved` — the
/// caller advances it as stages complete.
pub fn reserve(
    _state_dir: &Path,
    _from: &FromAddress,
    _to: &ToAddress,
    _amount: &UsdcAmount,
    _allow_retry: bool,
) -> CliResult<Record> {
    unimplemented!("idempotency::reserve — implement in third commit")
}

/// Persist a status/field update to the record's file. Overwrite-in-place;
/// no history preserved (we only need the latest for refuse-duplicate).
pub fn update(_state_dir: &Path, _record: &Record) -> CliResult<()> {
    unimplemented!("idempotency::update — implement in third commit")
}
