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
//! `$BRIDGE_WITHDRAW_STATE_DIR/<hex-digest>.json` (default:
//! `$HOME/.bridge-withdraw-state/`). Written on
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
    use std::os::unix::fs::OpenOptionsExt;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600) // in-flight money. NOT 0400 like --from-keys: the CLI
        // rewrites this file on every stage transition.
        .open(&path)
    {
        Ok(mut f) => {
            let record = fresh_reserved_record(&key, from, to, amount);
            // Any failure from here on must leave NO file behind: we own a
            // path that exists but holds nothing a later run can read, and
            // nothing has been sent. `commit` runs the fallible part so a
            // single cleanup covers all of it.
            let commit = |f: &mut std::fs::File| -> std::io::Result<()> {
                let body = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
                f.write_all(&body)?;
                // The burn goes out microseconds from now. Without this the
                // record is page cache, and a power loss between here and
                // the send lets the next run reserve cleanly and burn
                // again — the one failure this file exists to prevent.
                f.sync_all()?;
                // The file's bytes being durable does not make its *name*
                // durable. `create_new` added a directory entry, and that
                // entry needs its own fsync or a crash can leave the data
                // with nothing pointing at it.
                std::fs::File::open(state_dir)?.sync_all()?;
                Ok(())
            };
            if let Err(e) = commit(&mut f) {
                // Report what actually happened. The unlink can itself
                // fail — a read-only remount, a vanished directory — and
                // telling the operator "the partial record was removed"
                // when it is still sitting there sends them to the wrong
                // place: the next run will trip over a file they were told
                // was gone.
                let cleanup = match fs::remove_file(&path) {
                    Ok(()) => "the partial record was removed".to_string(),
                    Err(rm) => format!(
                        "WARNING: could not remove the partial record ({rm}) — delete {} by hand \
                         before re-running",
                        path.display()
                    ),
                };
                return Err(CliError::Preflight {
                    reason: format!(
                        "idempotency: reserve {}: {e} (nothing was sent; {cleanup})",
                        path.display()
                    ),
                    source: None,
                });
            }
            Ok(record)
        },
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
                },
            }
        },
        // Anything other than EEXIST — ENOSPC on a full state directory is
        // the realistic one. Nothing was sent here either, and that is the
        // one fact an operator needs before deciding whether to re-run, so
        // say it on every pre-send refusal rather than only the rewritten
        // ones.
        Err(e) => Err(CliError::Preflight {
            reason: format!(
                "idempotency: create {}: {e} (nothing was sent)",
                path.display()
            ),
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

/// Read the record for this withdrawal identity without creating one.
///
/// `reserve()` is a one-way door; the resume path needs to know whether a
/// prior run already broadcast *before* deciding to prompt and compose.
/// Missing file is `Ok(None)`, not an error.
pub fn peek(
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
) -> CliResult<Option<Record>> {
    let path = record_path(state_dir, &key(from, to, amount));
    if !path.exists() {
        return Ok(None);
    }
    read_record(&path).map(Some)
}

// -- helpers ------------------------------------------------------------

/// Create the state directory if it is missing, restrict what we create to
/// `0700`, and make the new levels durable.
///
/// The durability half is easy to miss. `reserve` fsyncs the record and
/// fsyncs `state_dir`, which makes the record's entry durable *inside*
/// `state_dir` — but if `state_dir` was created by this same run, its own
/// entry lives in *its* parent and has not been synced. A power loss can
/// then take the whole directory, record included, while the burn that
/// followed it landed. Syncing the innermost directory and not the path
/// that reaches it is a half-measure that looks complete.
///
/// The `if !state_dir.exists()` guard is load-bearing: an existing
/// directory the operator chose keeps whatever mode they gave it.
fn ensure_state_dir(state_dir: &Path) -> CliResult<()> {
    use std::os::unix::fs::PermissionsExt;
    if !state_dir.exists() {
        // Which levels are we about to bring into existence? Walk up to
        // the first ancestor that already exists; everything below it is
        // ours, and each one needs its parent's directory entry synced.
        let mut created: Vec<PathBuf> = Vec::new();
        let mut probe = state_dir;
        while !probe.exists() {
            created.push(probe.to_path_buf());
            match probe.parent() {
                Some(parent) => probe = parent,
                // Reached the filesystem root without finding anything
                // that exists. Nothing sane left to do; let create_dir_all
                // produce the real error.
                None => break,
            }
        }

        fs::create_dir_all(state_dir).map_err(|e| CliError::Preflight {
            reason: format!("idempotency: mkdir {}: {e}", state_dir.display()),
            source: None,
        })?;

        // Outermost first: `created` was collected innermost-first, so
        // reverse. Syncing "b" before "a" in a/b would order the entry for
        // b ahead of the entry for a that contains it.
        for level in created.iter().rev() {
            let Some(parent) = level.parent() else {
                continue;
            };
            // NOT best-effort. This runs before the reservation, which runs
            // before an irreversible burn; a directory entry we cannot make
            // durable is a reservation we cannot promise to find again.
            std::fs::File::open(parent)
                .and_then(|d| d.sync_all())
                .map_err(|e| CliError::Preflight {
                    reason: format!(
                        "idempotency: could not make {} durable (fsync of {}): {e}",
                        level.display(),
                        parent.display(),
                    ),
                    source: None,
                })?;
        }

        // Propagate, do not swallow: if we created the directory for
        // in-flight money and could not restrict it, the operator needs to
        // know now, not from a later audit.
        fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700)).map_err(|e| {
            CliError::Preflight {
                reason: format!(
                    "idempotency: created {} but could not restrict it to 0700: {e}",
                    state_dir.display()
                ),
                source: None,
            }
        })?;
    }
    Ok(())
}

/// `pub(crate)` so a post-burn failure can name the exact file an operator
/// has to edit to resume. The directory holds one record per withdrawal
/// identity, named by a SHA-256 nobody can compute by hand — "the state
/// file" without a path is not a recovery instruction.
pub(crate) fn record_path(state_dir: &Path, key: &str) -> PathBuf {
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
    let body = serde_json::to_vec_pretty(record).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: serialize record: {e}"),
        source: None,
    })?;

    // Same-directory tmp file so the rename is a same-filesystem atomic
    // op — cross-fs renames would fall back to copy+unlink.
    //
    // Randomly named, created 0600 by the OS, never colliding.
    //
    // Every hand-rolled scheme here is wrong in a way that only shows up
    // later: `.mode()` is ignored when opening an existing file, so a
    // predictable name lets a stale 0644 tmp ride the rename onto the
    // record; adding the pid still collides with this process's own
    // leftover after a failed write, and with a recycled pid after a
    // crash; adding a counter resets to zero on restart. `NamedTempFile`
    // draws a random name, retries on collision, and creates with mode
    // 0600 on Unix — which is exactly the property the rename has to carry
    // onto the record.
    let mut f = tempfile::NamedTempFile::new_in(state_dir).map_err(|e| CliError::Preflight {
        reason: format!(
            "idempotency: create temp file in {}: {e}",
            state_dir.display()
        ),
        source: None,
    })?;
    f.write_all(&body).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: write temp file for {}: {e}", dst.display()),
        source: None,
    })?;

    // Assert the mode rather than assume it — the guarantee is tempfile's,
    // this code depends on it, and `persist` consumes `f`, so this cannot
    // be checked afterwards.
    use std::os::unix::fs::PermissionsExt;
    debug_assert_eq!(
        f.as_file()
            .metadata()
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0),
        0o600,
        "NamedTempFile must create 0600; the rename carries this onto the record",
    );

    f.as_file().sync_all().map_err(|e| CliError::Preflight {
        reason: format!("idempotency: sync {}: {e}", dst.display()),
        source: None,
    })?;

    // `PersistError` carries the temp file back so it can be retried; we do
    // not, and dropping it here deletes the temp — which is what we want on
    // a failed rename.
    f.persist(dst).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: rename into {}: {}", dst.display(), e.error),
        source: None,
    })?;

    // The rename created a new directory entry; fsync the directory so the
    // entry survives a crash too. Best-effort here and load-bearing in
    // `reserve`: that one is the only write preceding an irreversible send,
    // so it is the only one where losing the entry costs money. A lost
    // stage-transition update costs a redundant re-check on resume.
    let _ = std::fs::File::open(state_dir).and_then(|d| d.sync_all());
    Ok(())
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

    #[test]
    fn reserve_still_refuses_a_hash_less_reserved_record() {
        // Regression guard for the double-spend this plan deliberately does
        // NOT introduce: `Reserved` + an_tx_hash == None is also the state a
        // burn that reached the wire and errored leaves behind, so it must
        // keep blocking a plain retry.
        let dir = TempDir::new().unwrap();
        let first = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        assert!(first.an_tx_hash.is_none());

        let second = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        );
        assert!(
            matches!(second, Err(CliError::DuplicateInFlight { .. })),
            "a hash-less Reserved record must still refuse without --allow-retry, got {second:?}",
        );
    }

    #[test]
    fn peek_is_read_only_and_reports_a_missing_record_as_none() {
        // `peek` runs before the confirmation prompt on every real run. If
        // it created anything, a declined prompt would leave the state file
        // that F4 exists to prevent — the bug would move, not close.
        let dir = TempDir::new().unwrap();
        let seen = peek(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
        )
        .unwrap();
        assert!(seen.is_none(), "no record yet");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "peek must not create a file",
        );

        let reserved = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        let seen = peek(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
        )
        .unwrap()
        .expect("the reserved record must be visible");
        assert_eq!(seen.key, reserved.key);
        assert_eq!(seen.status, Status::Reserved);
    }

    #[test]
    fn record_stays_owner_only_across_updates() {
        use std::os::unix::fs::PermissionsExt;
        // Must point at a path that does NOT exist yet: `TempDir` is itself
        // created 0700, so handing it straight to `reserve` skips the
        // `create_dir_all` + `set_permissions` branch entirely and the test
        // would assert on tempfile's behaviour instead of ours.
        let parent = TempDir::new().unwrap();
        let dir = parent.path().join("state");
        assert!(!dir.exists());
        let dir = dir.as_path();

        let mut rec = reserve(
            dir,
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        let path = dir.join(format!("{}.json", rec.key));

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "fresh reservation must be 0600, got {mode:04o}"
        );

        // The atomic writer renames a tmp file over the record — if the tmp is
        // created at the ambient umask, the mode is silently lost right here.
        rec.status = Status::Burned;
        rec.an_tx_hash = Some("0xdeadbeef".into());
        update(dir, &rec).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode must survive update(), got {mode:04o}");

        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "state dir we created must be 0700, got {dir_mode:04o}"
        );
    }

    #[test]
    fn a_stale_tmp_file_cannot_reinstate_0644() {
        // `.mode()` is a creation-time flag. If the writer reopens a
        // predictable leftover tmp instead of creating a fresh one, the stale
        // mode rides the rename back onto the record.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let mut rec = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();

        // Plant a world-readable leftover at the name the old predictable
        // scheme used. A writer that draws a random name neither inherits its
        // mode nor trips over it; the test deliberately does NOT try to guess
        // the new name, because a test that can predict it proves the name is
        // predictable.
        let stale = dir.path().join(format!(".{}.tmp", rec.key));
        std::fs::write(&stale, b"{}").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();

        rec.status = Status::Burned;
        rec.an_tx_hash = Some("0xdeadbeef".into());
        update(dir.path(), &rec).expect("a stale tmp must not break the write");

        let path = dir.path().join(format!("{}.json", rec.key));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a stale tmp must not leak its mode onto the record, got {mode:04o}"
        );
    }

    #[test]
    fn two_updates_from_one_process_do_not_collide() {
        // Any (key, pid[, counter]) scheme eventually reuses a name — after a
        // failed write, after a restart, after a recycled pid — and then
        // `create_new` turns a retry into a hard error.
        let dir = TempDir::new().unwrap();
        let mut rec = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        for (i, status) in [Status::Burned, Status::Captured, Status::Proved]
            .iter()
            .enumerate()
        {
            rec.status = *status;
            rec.an_tx_hash = Some(format!("0x{i:064x}"));
            update(dir.path(), &rec).expect("repeated updates from one process must succeed");
        }
    }

    #[test]
    fn an_operator_supplied_state_dir_keeps_its_own_mode() {
        // We tighten what we create; we do not re-chmod a directory the
        // operator chose. Silently changing the mode of an existing path is
        // not ours to do, and `--state-dir` may legitimately point at a
        // shared-group location.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();

        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "an existing state dir must be left alone, got {mode:04o}"
        );
    }

    #[test]
    fn an_unreadable_record_names_the_file_it_could_not_parse() {
        // The symptom F17's second hole produced. After the fix nothing writes
        // a partial record, but a hand-edited or externally truncated one is
        // still possible, and the operator must be told *which* file.
        let dir = TempDir::new().unwrap();
        let key = key(&sample_from(), &sample_to(), &UsdcAmount(500_000));
        std::fs::write(record_path(dir.path(), &key), b"").unwrap();

        let err = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .expect_err("an empty record must not be treated as absent");
        let msg = format!("{err}");
        assert!(msg.contains(&key[..8]), "must name the file, got: {msg}");
    }

    /// Total size of the filesystem holding `path`, or `None`.
    ///
    /// Defined here, in `idempotency`'s test module, and not borrowed from
    /// `preflight`: that one is private, lands in a later task, and a test in
    /// this module could not see it either way (E0425). Duplicating six lines
    /// of `statvfs` beats coupling this task's tests to that one's internals.
    fn fs_total_bytes(path: &std::path::Path) -> Option<u64> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a valid NUL-terminated path; `stat` is read only on
        // success.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        Some((stat.f_blocks as u64).saturating_mul(stat.f_frsize as u64))
    }

    /// A state dir on a filesystem with **no free space left**.
    ///
    /// `BRIDGE_TEST_FULL_FS` names a small writable mount (the CI job creates
    /// a 1 MiB tmpfs). Creating a directory there is not enough — the point is
    /// that the next `write_all` fails — so this fills the mount first.
    /// Returns `None` when the variable is unset, and the caller skips rather
    /// than passing vacuously.
    fn full_fs_state_dir() -> Option<std::path::PathBuf> {
        use std::io::Write;

        let base = std::path::PathBuf::from(std::env::var_os("BRIDGE_TEST_FULL_FS")?);

        // Two guards BEFORE writing a single byte. This helper's job is to
        // fill a filesystem until it refuses writes; pointed at the wrong path
        // it fills the developer's or the runner's root disk instead, and a
        // typo in a CI variable should not be able to do that.
        //
        //  1. It must be its own mount point, not a directory on a bigger filesystem.
        //     `st_dev` differing from the parent's is what makes a path a mount point.
        //  2. It must be small. A 1 MiB tmpfs is what the job provisions; a cap well
        //     above that and far below any real disk catches both a stale variable and
        //     a well-meaning larger tmpfs.
        const MAX_FS_BYTES: u64 = 64 * 1024 * 1024;

        let is_mount_point = {
            use std::os::unix::fs::MetadataExt;
            match (
                std::fs::metadata(&base),
                base.parent().map(std::fs::metadata),
            ) {
                (Ok(here), Some(Ok(up))) => here.dev() != up.dev(),
                // No parent means "/", which is emphatically not what we want.
                _ => false,
            }
        };
        assert!(
            is_mount_point,
            "BRIDGE_TEST_FULL_FS={} is not a mount point. This test fills the filesystem it is \
             given; refusing to do that to a shared one.",
            base.display()
        );

        let total = fs_total_bytes(&base).unwrap_or(u64::MAX);
        assert!(
            total <= MAX_FS_BYTES,
            "BRIDGE_TEST_FULL_FS={} is {:.1} MB — too large to fill safely (cap {} MB). Point it \
             at a small dedicated tmpfs.",
            base.display(),
            total as f64 / 1e6,
            MAX_FS_BYTES / 1_000_000,
        );

        // Past the env check every failure is a failure. `.ok()?` here would
        // return `None`, and the caller reads `None` as "this host was not
        // asked to run the test" — turning a broken fixture into a green skip
        // on precisely the machine that was configured to run it.
        let dir = base.join(format!("s{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));

        // Fill the mount completely. No slack.
        //
        // Reserving a slack file and deleting it was an attempt to land the
        // failure on `write_all` specifically, and it cannot work: the
        // smallest file that frees a data block is one block (4096 B), and a
        // serialised `Record` is a few hundred bytes — so releasing a block
        // always leaves room for the record, `reserve` succeeds, and the
        // helper's own probe then reports "not full" and skips. The test was
        // guaranteed to be skipped, and a skip read as a pass.
        //
        // On tmpfs and ext4, filling data blocks still leaves inodes, so
        // `create_new` typically succeeds and `write_all` fails — which is the
        // interesting path. But it is not guaranteed, and the test does not
        // need it to be: see the invariant it actually asserts.
        let ballast = base.join(format!("ballast{}", std::process::id()));
        if let Ok(mut f) = std::fs::File::create(&ballast) {
            let chunk = vec![0u8; 64 * 1024];
            // ENOSPC here is the success condition, not an error.
            while f.write_all(&chunk).is_ok() {}
            let _ = f.sync_all();
        }

        // Confirm we really are out of room. A few hundred bytes is the size
        // that matters, so probe with that rather than a round number.
        let probe = dir.join("probe.bin");
        let full = matches!(
            std::fs::write(&probe, vec![0u8; 512]),
            Err(ref e) if e.raw_os_error() == Some(libc::ENOSPC)
        );
        let _ = std::fs::remove_file(&probe);
        if !full {
            // The variable was set, so someone meant this to run. Failing is
            // right: a filesystem that will not fill means the fixture is
            // broken, not that the machine lacks a prerequisite — that case is
            // the `None` return above, before any of this.
            panic!(
                "BRIDGE_TEST_FULL_FS={} could not be filled; the ENOSPC test cannot run",
                base.display()
            );
        }
        Some(dir)
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_FULL_FS; runs in test:rust:ackinacki-bridge:enospc"]
    fn a_reserve_that_fails_leaves_no_record_behind() {
        // Needs a filesystem that can actually run out of space mid-write.
        // `BRIDGE_TEST_FULL_FS` names a small writable mount; the helper fills
        // it, so the failure is a real ENOSPC from `write_all` rather than a
        // simulation.
        let Some(dir) = full_fs_state_dir() else {
            eprintln!("skipping: BRIDGE_TEST_FULL_FS unset (see the CI mount)");
            return;
        };
        // The invariant, stated so it does not depend on which syscall failed:
        // a reservation that cannot be made durable must refuse, must say
        // nothing was sent, and must leave no record for the next run to trip
        // over. Whether ENOSPC hit `create_new` or `write_all` is the
        // filesystem's business — both are real, and both must behave.
        let err = reserve(
            &dir,
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .expect_err("a full filesystem must not report a successful reservation");
        assert!(
            format!("{err}").contains("nothing was sent"),
            "the refusal must say nothing left the machine, got: {err}"
        );

        // Count RECORD files, not directory entries. Two reasons: the helper's
        // own probe file may survive an ENOSPC (`fs::write` can create the
        // inode and then fail on the body, leaving a zero-length file), and
        // `full_fs_state_dir` deliberately leaves the ballast on the same
        // mount. Asserting `read_dir(..).count() == 0` would fail on litter
        // that has nothing to do with what is being tested.
        let records = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count();
        assert_eq!(
            records, 0,
            "a failed reserve must leave no partial record for the next run to trip over"
        );
    }
}
