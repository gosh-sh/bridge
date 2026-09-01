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

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::args::{FromAddress, ToAddress, UsdcAmount};
use crate::errors::{CliError, CliResult};

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

/// Try to reserve the record. Behavior:
/// - No prior record → fresh `Status::Reserved` written and returned.
/// - Prior `Status::Failed` with `an_tx_hash` present → resume: return
///   the prior record verbatim so the orchestrator sees the recorded AN
///   tx hash and skips `burn::fire()`. Wiping here would drop the hash
///   and cause a second `initiateWithdrawal` broadcast — a double-spend
///   on the AN side. The only writer of `Status::Failed` in production
///   is the `withdrawByProof` revert path, which by construction only
///   runs after a successful burn, so a `Failed` record without an
///   `an_tx_hash` is a manual-edit or future-writer edge case.
/// - Prior `Status::Failed` without `an_tx_hash` → no burn happened yet
///   (defensive branch): safe to wipe and start fresh.
/// - Prior `Status::Confirmed` → always refused. The withdrawal already
///   paid out; retrying it would either be a no-op or a double-broadcast.
///   To move funds again, use a new identity (different amount/recipient).
/// - Prior `Status::Submitted` → always refused, even with `--allow-retry`.
///   The tx was broadcast but we did not observe the receipt; blindly
///   re-broadcasting is a double-spend risk. Reconcile the prior
///   `eth_tx_hash` on-chain, then either wait or mark the record `Failed`
///   manually.
/// - Any other active status (`Reserved`, `Burned`, `Captured`, `Proved`) →
///   refused unless `--allow-retry` is set. With the flag, the **prior
///   record is returned as-is** — `an_tx_hash`, `withdrawal_msg_id`,
///   `block_seq_no`, `proof_json_path` are all preserved so the
///   orchestrator can skip stages that already completed. This is v1's
///   resume path (a `--resume` alias may be added later).
pub fn reserve(
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    allow_retry: bool,
) -> CliResult<Record> {
    ensure_state_dir(state_dir)?;

    let key = key(from, to, amount);
    let path = record_path(state_dir, &key);

    // First-writer-wins probe: create_new is atomic on POSIX + Windows
    // (fails EEXIST rather than truncating). If it succeeds we own a fresh
    // reservation; if it fails EEXIST we need to inspect the prior record.
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            let record = fresh_reserved_record(&key, from, to, amount);
            let body = serde_json::to_vec_pretty(&record).map_err(|e| {
                CliError::Preflight {
                    reason: format!("idempotency: serialize reserved record: {e}"),
                    source: None,
                }
            })?;
            f.write_all(&body).map_err(|e| CliError::Preflight {
                reason: format!("idempotency: write {}: {e}", path.display()),
                source: None,
            })?;
            Ok(record)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let prior = read_record(&path)?;
            match prior.status {
                // Terminal states — refuse regardless of --allow-retry.
                // Confirmed already paid out; Submitted has an unresolved
                // in-flight tx and re-broadcasting is a double-spend risk.
                Status::Confirmed | Status::Submitted => {
                    Err(CliError::DuplicateInFlight {
                        prior_status: format!("{:?}", prior.status).to_ascii_lowercase(),
                        prior_tx: prior.an_tx_hash,
                        prior_msg_id: prior.withdrawal_msg_id,
                    })
                }
                // Failed → the only production writer sets this after
                // `withdrawByProof` reverts on an already-broadcast burn,
                // so a stored `an_tx_hash` means "AN burn is already
                // done". Return the prior record verbatim in that case
                // so the orchestrator's resume branch (`prior_an_tx =
                // record.an_tx_hash.clone()`) skips `burn::fire()`
                // instead of firing a second `initiateWithdrawal`.
                // Only wipe when there is no recorded AN tx (defensive:
                // manual-edited state file, hypothetical future writer
                // that marks Failed pre-burn).
                Status::Failed => {
                    if prior.an_tx_hash.is_some() {
                        Ok(prior)
                    } else {
                        let record = fresh_reserved_record(&key, from, to, amount);
                        write_record_atomic(state_dir, &path, &record)?;
                        Ok(record)
                    }
                }
                // Active resumable states.
                Status::Reserved | Status::Burned | Status::Captured | Status::Proved => {
                    if allow_retry {
                        // Resume: return the prior record verbatim so the
                        // orchestrator can skip stages by inspecting fields
                        // like `an_tx_hash` / `withdrawal_msg_id`. Do NOT
                        // overwrite with a fresh Reserved — that would drop
                        // the stored AN tx hash and cause an unconditional
                        // re-burn (double-spend on the AN side).
                        Ok(prior)
                    } else {
                        Err(CliError::DuplicateInFlight {
                            prior_status: format!("{:?}", prior.status).to_ascii_lowercase(),
                            prior_tx: prior.an_tx_hash,
                            prior_msg_id: prior.withdrawal_msg_id,
                        })
                    }
                }
            }
        }
        Err(e) => Err(CliError::Preflight {
            reason: format!("idempotency: create {}: {e}", path.display()),
            source: None,
        }),
    }
}

/// Persist a status/field update to the record's file. Overwrite-in-place
/// via write-temp + rename so a mid-write crash never leaves a torn file.
/// No history preserved — we only need the latest for refuse-duplicate.
pub fn update(state_dir: &Path, record: &Record) -> CliResult<()> {
    ensure_state_dir(state_dir)?;
    let path = record_path(state_dir, &record.key);
    write_record_atomic(state_dir, &path, record)
}

// -- helpers ------------------------------------------------------------

fn ensure_state_dir(state_dir: &Path) -> CliResult<()> {
    fs::create_dir_all(state_dir).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: mkdir {}: {e}", state_dir.display()),
        source: None,
    })
}

fn record_path(state_dir: &Path, key: &str) -> PathBuf {
    state_dir.join(format!("{key}.json"))
}

fn fresh_reserved_record(
    key: &str,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
) -> Record {
    Record {
        key: key.to_string(),
        status: Status::Reserved,
        from_extended: from.extended(),
        to_hex: format!("0x{}", hex::encode(to.address.as_slice())),
        to_chain: to.chain_id,
        amount_micro: amount.0,
        reserved_at: rfc3339_now(),
        an_tx_hash: None,
        withdrawal_msg_id: None,
        block_seq_no: None,
        proof_json_path: None,
        eth_tx_hash: None,
    }
}

fn read_record(path: &Path) -> CliResult<Record> {
    let bytes = fs::read(path).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: read {}: {e}", path.display()),
        source: None,
    })?;
    serde_json::from_slice::<Record>(&bytes).map_err(|e| CliError::Preflight {
        reason: format!(
            "idempotency: prior record at {} is corrupt: {e} \
             (delete it manually if you know it's stale)",
            path.display()
        ),
        source: None,
    })
}

fn write_record_atomic(state_dir: &Path, dst: &Path, record: &Record) -> CliResult<()> {
    // Same-directory tmp file so the rename is a same-filesystem atomic
    // op — cross-fs renames would fall back to copy+unlink.
    let tmp = state_dir.join(format!(".{}.tmp", record.key));
    let body = serde_json::to_vec_pretty(record).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: serialize record: {e}"),
        source: None,
    })?;
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| CliError::Preflight {
                reason: format!("idempotency: open tmp {}: {e}", tmp.display()),
                source: None,
            })?;
        f.write_all(&body).map_err(|e| CliError::Preflight {
            reason: format!("idempotency: write tmp {}: {e}", tmp.display()),
            source: None,
        })?;
        f.sync_all().map_err(|e| CliError::Preflight {
            reason: format!("idempotency: fsync tmp {}: {e}", tmp.display()),
            source: None,
        })?;
    }
    fs::rename(&tmp, dst).map_err(|e| CliError::Preflight {
        reason: format!(
            "idempotency: rename {} -> {}: {e}",
            tmp.display(),
            dst.display()
        ),
        source: None,
    })
}

/// Epoch-seconds RFC 3339 UTC timestamp without a dep on `chrono`/`time`.
/// Format: `1970-01-01T00:00:00Z` (no fractional seconds — this record is
/// for reconciliation, not perf tracing).
fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    format_utc(secs)
}

// Trivial UTC seconds → "YYYY-MM-DDTHH:MM:SSZ". Uses the standard civil
// calendar formula (Howard Hinnant's date algorithms, days-since-epoch
// variant). Handles all 32-bit years plus a safety margin without any
// external deps.
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Days from civil (see http://howardhinnant.github.io/date_algorithms.html)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use std::str::FromStr;
    use tempfile::TempDir;

    fn sample_from() -> FromAddress {
        FromAddress {
            dapp_id_hex: "a".repeat(64),
            account_id_hex: "b".repeat(64),
        }
    }

    fn sample_to() -> ToAddress {
        ToAddress {
            address: Address::from_str("0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f").unwrap(),
            chain_id: 11_155_111,
        }
    }

    #[test]
    fn key_is_deterministic_and_covers_all_identity_fields() {
        let k1 = key(&sample_from(), &sample_to(), &UsdcAmount(1_000_000));
        let k2 = key(&sample_from(), &sample_to(), &UsdcAmount(1_000_000));
        assert_eq!(k1, k2, "same identity → same key");
        assert_eq!(k1.len(), 64, "sha256 hex");

        let k3 = key(&sample_from(), &sample_to(), &UsdcAmount(2_000_000));
        assert_ne!(k1, k3, "amount participates in the digest");

        let mut other_from = sample_from();
        other_from.account_id_hex = "c".repeat(64);
        let k4 = key(&other_from, &sample_to(), &UsdcAmount(1_000_000));
        assert_ne!(k1, k4, "from participates in the digest");

        let mut other_to = sample_to();
        other_to.chain_id = 1;
        let k5 = key(&sample_from(), &other_to, &UsdcAmount(1_000_000));
        assert_ne!(k1, k5, "chain_id participates in the digest");
    }

    #[test]
    fn reserve_first_time_writes_a_fresh_record() {
        let dir = TempDir::new().unwrap();
        let rec = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .expect("first reserve should succeed");
        assert_eq!(rec.status, Status::Reserved);
        assert!(rec.an_tx_hash.is_none());
        assert!(dir.path().join(format!("{}.json", rec.key)).exists());
    }

    #[test]
    fn reserve_duplicate_active_refuses() {
        let dir = TempDir::new().unwrap();
        let _first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        let second = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false);
        match second {
            Err(CliError::DuplicateInFlight { prior_status, .. }) => {
                assert_eq!(prior_status, "reserved");
            }
            other => panic!("expected DuplicateInFlight, got {other:?}"),
        }
    }

    #[test]
    fn reserve_allows_retry_over_active_when_flag_set() {
        let dir = TempDir::new().unwrap();
        let _first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        let second = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), true)
            .expect("--allow-retry should override refuse-duplicate");
        // Prior was Reserved with no downstream fields → status stays Reserved.
        assert_eq!(second.status, Status::Reserved);
    }

    #[test]
    fn reserve_with_retry_preserves_prior_fields_for_resume() {
        // Regression: v1 --allow-retry used to overwrite the prior record
        // with a fresh Reserved, dropping `an_tx_hash`. That caused the
        // orchestrator to re-fire burn on a withdrawal that had already
        // broadcast — a double-spend on the AN side. Resume semantics must
        // hand the caller back the prior record verbatim.
        let dir = TempDir::new().unwrap();
        let mut first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        first.status = Status::Burned;
        first.an_tx_hash = Some("0xdeadbeef".into());
        update(dir.path(), &first).unwrap();

        let resumed = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), true)
            .expect("--allow-retry over Burned should resume");
        assert_eq!(resumed.status, Status::Burned, "prior status preserved");
        assert_eq!(
            resumed.an_tx_hash.as_deref(),
            Some("0xdeadbeef"),
            "prior an_tx_hash must survive so orchestrator can skip re-burn"
        );
    }

    #[test]
    fn reserve_refuses_confirmed_even_with_retry() {
        let dir = TempDir::new().unwrap();
        let mut first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        first.status = Status::Confirmed;
        first.eth_tx_hash = Some("0xabc".into());
        update(dir.path(), &first).unwrap();

        let attempt = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), true);
        assert!(
            matches!(attempt, Err(CliError::DuplicateInFlight { .. })),
            "Confirmed is terminal — --allow-retry must not resurrect it, got {attempt:?}"
        );
    }

    #[test]
    fn reserve_refuses_submitted_even_with_retry() {
        // Submitted = we broadcast an EVM tx but never observed the receipt.
        // Re-broadcasting without reconciling risks paying twice.
        let dir = TempDir::new().unwrap();
        let mut first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        first.status = Status::Submitted;
        first.eth_tx_hash = Some("0xpending".into());
        update(dir.path(), &first).unwrap();

        let attempt = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), true);
        assert!(
            matches!(attempt, Err(CliError::DuplicateInFlight { .. })),
            "Submitted must refuse until reconciled, got {attempt:?}"
        );
    }

    #[test]
    fn reserve_over_failed_without_an_tx_hash_wipes_to_reserved() {
        // Defensive branch: a `Failed` record with no `an_tx_hash` means
        // no burn happened (only reachable via manual edit or a future
        // writer that marks Failed pre-burn). Safe to wipe and start
        // fresh without `--allow-retry`.
        let dir = TempDir::new().unwrap();
        let mut first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        first.status = Status::Failed;
        // an_tx_hash intentionally left None
        update(dir.path(), &first).unwrap();

        let second = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .expect("Failed without an_tx_hash is retryable without --allow-retry");
        assert_eq!(second.status, Status::Reserved);
        assert!(second.an_tx_hash.is_none());
    }

    #[test]
    fn reserve_over_failed_with_an_tx_hash_preserves_prior_record() {
        // Regression (Sergey review 2026-09-01, «Failed всё ещё жжёт ECC
        // второй раз»): the sole production writer of Status::Failed is
        // the withdrawByProof-revert arm of the orchestrator, which only
        // fires after a successful AN burn. If reserve() wiped the
        // record on Failed, the stored `an_tx_hash` would be dropped and
        // the orchestrator's Some(existing) resume branch would miss —
        // firing a SECOND `initiateWithdrawal` on retry (double-burn of
        // ECC[3] on the AN side). Retry after Case 3e (top-up-treasury,
        // re-run) must resume without --allow-retry.
        let dir = TempDir::new().unwrap();
        let mut first = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        first.status = Status::Failed;
        first.an_tx_hash = Some("0xdeadbeef".into());
        first.withdrawal_msg_id = Some("0xmsg".into());
        first.block_seq_no = Some(12345);
        update(dir.path(), &first).unwrap();

        let second = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .expect("Failed with an_tx_hash must resume without --allow-retry");
        assert_eq!(second.status, Status::Failed, "prior status preserved for resume");
        assert_eq!(
            second.an_tx_hash.as_deref(),
            Some("0xdeadbeef"),
            "an_tx_hash must survive so orchestrator skips re-burn",
        );
        assert_eq!(second.withdrawal_msg_id.as_deref(), Some("0xmsg"));
        assert_eq!(second.block_seq_no, Some(12345));
    }

    #[test]
    fn update_persists_stage_transition() {
        let dir = TempDir::new().unwrap();
        let mut rec = reserve(dir.path(), &sample_from(), &sample_to(), &UsdcAmount(500_000), false)
            .unwrap();
        rec.status = Status::Burned;
        rec.an_tx_hash = Some("0xdeadbeef".into());
        update(dir.path(), &rec).unwrap();

        let disk = read_record(&record_path(dir.path(), &rec.key)).unwrap();
        assert_eq!(disk.status, Status::Burned);
        assert_eq!(disk.an_tx_hash.as_deref(), Some("0xdeadbeef"));
    }

    #[test]
    fn format_utc_matches_known_epoch_boundaries() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
