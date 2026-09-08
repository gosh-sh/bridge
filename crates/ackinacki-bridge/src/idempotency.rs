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

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{
    args::{FromAddress, ToAddress, UsdcAmount},
    errors::{CliError, CliResult},
};

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

impl Status {
    /// Statuses that forbid any further work on this identity, whatever
    /// flags the run passes. `Confirmed` has already paid out; `Submitted`
    /// has a broadcast EVM transaction whose receipt nobody has seen, so
    /// re-broadcasting risks a double payout.
    ///
    /// One list, because two places need it: [`reserve`], and the resume
    /// path — which restores a record deleted mid-preflight and must then
    /// answer exactly as `reserve` would have if the file had survived.
    /// Hardcoding a status in the second of those turned a paid-out
    /// withdrawal into a `burned` one and carried it through a second
    /// `withdrawByProof`.
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Confirmed | Status::Submitted)
    }
}

/// The refusal a record in a terminal status earns, and `None` for every
/// other status — so this and [`Status::is_terminal`] cannot drift apart.
///
/// Exit 3. `--allow-retry` does NOT reach these, so the shared "re-run with
/// --allow-retry to override" the message used to end on was wrong here: it
/// named the one flag that changes nothing about a terminal record.
#[must_use]
fn terminal_refusal(record: &Record) -> Option<CliError> {
    if !record.status.is_terminal() {
        return None;
    }
    let remedy = if record.status == Status::Confirmed {
        "This withdrawal already paid out. `--allow-retry` does not reopen it. To move funds \
         again, use a different (amount, recipient, chain) — the identity is what the record is \
         keyed on."
    } else {
        "There is a broadcast EVM transaction whose receipt was never observed, and \
         `--allow-retry` does not override that: re-broadcasting risks a double payout. Reconcile \
         eth_tx_hash on chain first, then either wait for a run to see the receipt or set the \
         record to \"failed\" by hand."
    };
    Some(CliError::DuplicateInFlight {
        prior_status: format!("{:?}", record.status).to_ascii_lowercase(),
        prior_tx: record.an_tx_hash.clone(),
        prior_msg_id: record.withdrawal_msg_id.clone(),
        remedy: remedy.to_string(),
    })
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
    pub eth_tx_hash: Option<String>,
}

/// Which side of the atomic `create_new` this run came out on.
///
/// `reserve` used to return a bare `Record` for both, and the caller could
/// not tell them apart. That is the concurrent double-burn: two runs with
/// `--allow-retry`, no prior record, both `peek` → `None`. A wins the
/// `create_new` and enters the multi-second `burn::send`; B gets EEXIST,
/// reads A's record — `Reserved`, `an_tx_hash` still `None` because A has
/// not returned yet — and, seeing no hash, decides to send. A second
/// `initiateWithdrawal` against a multisig with no replay guard.
///
/// No amount of inspecting the record's *fields* distinguishes those two
/// states, because the field that would (`an_tx_hash`) is populated only
/// after the send returns. The provenance has to be carried out of the
/// syscall that knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reservation {
    /// This run won `create_new`: the identity is ours and nothing else
    /// holds it.
    Created,
    /// The record already existed. Another run owns this identity — either
    /// still in flight, or finished and being resumed.
    Found,
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
/// - Prior `Status::Failed` with `an_tx_hash` present → resume: return the
///   prior record verbatim so the orchestrator sees the recorded AN tx hash and
///   skips `burn::compose`/`burn::send`. Wiping here would drop the hash and
///   cause a second `initiateWithdrawal` broadcast — a double-spend on the AN
///   side. The only writer of `Status::Failed` in production is the
///   `withdrawByProof` revert path, which by construction only runs after a
///   successful burn, so a `Failed` record without an `an_tx_hash` is a
///   manual-edit or future-writer edge case.
/// - Prior `Status::Failed` without `an_tx_hash` → REFUSED by [`read_record`]
///   before this function chooses anything. There is no production writer of
///   that combination — `Failed` is written only by the post-burn
///   `withdrawByProof` revert — so it is a hand-edited or corrupt record, and
///   the branch that used to wipe it and report a fresh reservation is gone. It
///   published with `rename`, which excludes nobody, while calling the result
///   `Created`: two runs could both wipe and both burn.
/// - Prior `Status::Confirmed` → always refused. The withdrawal already paid
///   out; retrying it would either be a no-op or a double-broadcast. To move
///   funds again, use a new identity (different amount/recipient).
/// - Prior `Status::Submitted` → always refused, even with `--allow-retry`. The
///   tx was broadcast but we did not observe the receipt; blindly
///   re-broadcasting is a double-spend risk. Reconcile the prior `eth_tx_hash`
///   on-chain, then either wait or mark the record `Failed` manually.
/// - Any other active status (`Reserved`, `Burned`, `Captured`, `Proved`) →
///   refused unless `--allow-retry` is set. With the flag, the **prior record
///   is returned as-is** — `an_tx_hash`, `withdrawal_msg_id` and `block_seq_no`
///   are all preserved so the orchestrator can skip stages that already
///   completed. This is v1's resume path (a `--resume` alias may be added
///   later).
pub fn reserve(
    state_dir: &Path,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    allow_retry: bool,
) -> CliResult<(Record, Reservation)> {
    ensure_state_dir(state_dir)?;

    let key = key(from, to, amount);
    let path = record_path(state_dir, &key);

    // First-writer-wins, publishing the name and the contents together.
    //
    // `create_new` alone gives exclusion but NOT atomicity of content: it
    // makes the file exist, empty, and only then does the write land. A
    // racing run that hits EEXIST inside that window reads zero bytes and
    // is told "prior record is corrupt … delete it manually if you know
    // it's stale" — advice which, followed, deletes the only guard against
    // a second burn while the first run is still inside `burn::send`.
    // (Found by `only_one_of_many_racing_reservations_creates`; every
    // earlier test called `reserve` sequentially, where the window cannot
    // be observed.)
    //
    // `hard_link` is the POSIX primitive that does both: it fails EEXIST
    // when the destination exists, and when it succeeds the destination
    // already carries the content. So build a complete, synced temp file
    // first, then link it into place. `NamedTempFile` creates 0600 — in-
    // flight money, and NOT 0400 like `--from-keys`, because the CLI
    // rewrites this file on every stage transition — and deletes itself on
    // every error path, so a failure here can no longer leave a partial
    // record behind at all.
    let record = fresh_reserved_record(&key, from, to, amount);
    // Whether the record reached its name. Everything after the
    // `hard_link` can still fail, and the two sides of that need different
    // sentences: before it, nothing exists and nothing was left behind;
    // after it, a COMPLETE record is on disk and other processes can
    // already see it.
    let published = std::cell::Cell::new(false);
    let publish = || -> std::io::Result<bool> {
        let mut tmp = tempfile::NamedTempFile::new_in(state_dir)?;
        let body = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
        tmp.write_all(&body)?;
        // The burn goes out microseconds from now. Without this the record
        // is page cache, and a power loss between here and the send lets
        // the next run reserve cleanly and burn again — the one failure
        // this file exists to prevent.
        tmp.as_file().sync_all()?;
        match std::fs::hard_link(tmp.path(), &path) {
            Ok(()) => published.set(true),
            // Someone else won the identity. Their record is complete by
            // construction, so the read below sees whole JSON.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(e),
        }
        // The file's bytes being durable does not make its *name* durable.
        // The link added a directory entry, and that entry needs its own
        // fsync or a crash can leave the data with nothing pointing at it.
        std::fs::File::open(state_dir)?.sync_all()?;
        Ok(true)
    };
    let won = publish().map_err(|e| CliError::Preflight {
        // "No partial record was left behind" was true of a torn file and
        // false of the case that actually reaches here: the `hard_link`
        // succeeded and the directory fsync did not. An operator read that
        // as "nothing is on disk", and the next run then refused with exit
        // 3 about a record they had been told did not exist — straight
        // into deleting it.
        //
        // The record is NOT removed in that case, and deliberately. It is
        // complete and already visible to other processes; unlinking a
        // published reservation is the double-burn this file exists to
        // prevent. Say what is there instead.
        reason: if published.get() {
            format!(
                "idempotency: reserve {}: {e}\n\x20 Nothing was sent. The record IS on disk and \
                 complete — the failure was making its directory entry durable, which means it \
                 may not survive a power loss.\n\x20 It is not stale: a later run refusing this \
                 identity with exit 3 is correct, and that refusal explains what to do with it.",
                path.display()
            )
        } else {
            format!(
                "idempotency: reserve {}: {e} (nothing was sent; no record was left behind)",
                path.display()
            )
        },
        source: None,
    })?;

    if won {
        return Ok((record, Reservation::Created));
    }

    // EEXIST: inspect whoever got there first. Their record is complete by
    // construction — `hard_link` only publishes a fully written file — so
    // this read cannot see a torn one.
    let prior = read_record(&path)?;
    match prior.status {
        // Terminal states — refuse regardless of --allow-retry.
        // Confirmed already paid out; Submitted has an unresolved
        // in-flight tx and re-broadcasting is a double-spend risk.
        Status::Confirmed | Status::Submitted => Err(terminal_refusal(&prior)
            .expect("Status::is_terminal names exactly the statuses in this arm")),
        // Failed → the only production writer sets this after
        // `withdrawByProof` reverts on an already-broadcast burn,
        // so a stored `an_tx_hash` means "AN burn is already
        // done". Return the prior record verbatim in that case
        // so the orchestrator's resume branch (`prior_an_tx =
        // record.an_tx_hash.clone()`) skips `burn::send`
        // instead of firing a second `initiateWithdrawal`.
        Status::Failed => {
            // `Found`, unconditionally, and there is no longer a branch
            // that wipes.
            //
            // The old one fired when `an_tx_hash` was absent, on the
            // stated grounds that "`Failed` without a hash is only ever
            // written by a pre-burn path". No such path exists — the sole
            // production writer of `Failed` is the post-burn
            // `withdrawByProof` revert — so the branch only ever ran on a
            // hand-edited or corrupt record, and it answered by writing a
            // fresh record with `rename` and reporting `Created`.
            //
            // That is the whole invariant broken in one line. `Created` is
            // the caller's evidence that THIS run won the atomic publish,
            // and `rename` excludes nobody: two runs reading the same
            // record could both wipe, both be told they created it, and
            // both burn. `read_record` now refuses that record outright,
            // so the case cannot reach here at all — and if a future edit
            // ever lets it through, `Found` is still the safe answer,
            // because `decide_burn` turns it into exit 3 rather than a
            // second `initiateWithdrawal`.
            Ok((prior, Reservation::Found))
        },
        // Active resumable states.
        //
        // The second condition is about the HASH, not the status, and it
        // is what stops this arm handing out advice that leads nowhere.
        // Only a record with a hash is resumable, and only for that one is
        // `--allow-retry` the answer; a hash-less record refuses the flag
        // outright further down the line. Naming the flag anyway sent an
        // operator from this refusal to another one whose text is
        // "--allow-retry does NOT override this", which is the dead end
        // the flag advice was moved out of a round ago — it just moved
        // here.
        //
        // So a hash-less record is handed back for `decide_burn` to
        // refuse. That is not a weakening: `(None, Found)` is exactly the
        // combination it rejects, and it is the caller that knows whether
        // this run holds the withdrawal lock — which is the half of the
        // answer this function cannot supply and the refusal needs.
        Status::Reserved | Status::Burned | Status::Captured | Status::Proved => {
            if allow_retry || prior.an_tx_hash.is_none() {
                // Resume: return the prior record verbatim so the
                // orchestrator can skip stages by inspecting fields
                // like `an_tx_hash` / `withdrawal_msg_id`. Do NOT
                // overwrite with a fresh Reserved — that would drop
                // the stored AN tx hash and cause an unconditional
                // re-burn (double-spend on the AN side).
                Ok((prior, Reservation::Found))
            } else {
                Err(CliError::DuplicateInFlight {
                    prior_status: format!("{:?}", prior.status).to_ascii_lowercase(),
                    prior_tx: prior.an_tx_hash,
                    prior_msg_id: prior.withdrawal_msg_id,
                    // Here the flag IS the answer: this record carries a
                    // hash, so with the flag it resumes at capture.
                    remedy: "Reconcile via GraphQL, or re-run with --allow-retry to resume from \
                             the recorded burn."
                        .to_string(),
                })
            }
        },
    }
}

/// Persist a status/field update to the record's file. Overwrite-in-place
/// via write-temp + rename so a mid-write crash never leaves a torn file.
/// No history preserved — we only need the latest for refuse-duplicate.
/// A state-file write that did not land.
///
/// **Deliberately not convertible into [`CliError`].** There is no `From`
/// impl, so `idempotency::update(..)?` inside a function returning
/// `CliResult` is a compile error. That is the entire point of the type.
///
/// The right exit code depends on a fact the author has to supply and
/// cannot be trusted to remember: whether anything has already gone on the
/// wire. Exit 2 is published as "refused before sending, nothing left the
/// machine"; after a burn — let alone after `withdrawByProof` has paid out
/// — that is a false statement to an operator and to every script matching
/// on the contract. Four call sites drifted into `?` exactly because the
/// compiler had no opinion. Now it does: pick [`Self::before_send`] or
/// [`Self::after_send`] at each one.
#[derive(Debug)]
pub struct UpdateFailed {
    record_path: PathBuf,
    reason: String,
}

impl UpdateFailed {
    /// Nothing has been broadcast for this withdrawal yet, so this is an
    /// ordinary pre-send refusal: exit 2, and re-running is safe.
    /// `#[allow(dead_code)]` outside tests, deliberately. Every `update`
    /// in the pipeline today happens after the burn, so nothing in the
    /// release build calls this — but its absence would leave `after_send`
    /// as the only option, and an author with a genuinely pre-send write
    /// would then either mislabel it or reach for a `From` impl and undo
    /// the whole guard. The test exercises it.
    ///
    /// Keeping it costs one thing: it compiles at the post-burn sites too,
    /// where it turns a burn on the wire into exit 2. That is what
    /// `orchestrator`'s
    /// `no_state_write_in_this_pipeline_claims_that_nothing_was_sent`
    /// watches for — every `update` in the withdraw pipeline is downstream
    /// of the send, so the guard there refuses this constructor outright.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn before_send(self) -> CliError {
        CliError::Preflight {
            reason: format!(
                "idempotency: could not write {}: {} (nothing was sent)",
                self.record_path.display(),
                self.reason,
            ),
            source: None,
        }
    }

    /// Something is already on the wire. `what_landed` names it, because
    /// "the state file could not be written" without saying what it was
    /// meant to record leaves the operator unable to reconstruct it.
    ///
    /// Exit 10 rather than 2: the defining fact is that value moved and our
    /// record of it is not durable, so the operator must reconcile on chain
    /// before re-running. It is deliberately the same code whether the
    /// payout has happened or not — "sent, and the local record is behind"
    /// is one operational state, and the message carries the detail.
    pub fn after_send(self, what_landed: &str) -> CliError {
        CliError::BurnOutcomeUnknown {
            reason: format!(
                "{what_landed}, but the state file could not be updated: {}\n\x20 The local \
                 record is now BEHIND the chain. Do not re-run without reconciling — see the \
                 runbook's Case 3a.\n\x20 The record that needed writing is {}.",
                self.reason,
                self.record_path.display(),
            ),
            source: None,
        }
    }
}

/// Persist a status/field update to the record's file.
///
/// Returns [`UpdateFailed`], which does not convert into `CliError` — see
/// that type for why. Every caller must say whether anything has been
/// broadcast yet.
pub fn update(state_dir: &Path, record: &Record) -> Result<(), UpdateFailed> {
    let path = record_path(state_dir, &record.key);
    let fail = |e: CliError| UpdateFailed {
        record_path: path.clone(),
        reason: format!("{e}"),
    };
    ensure_state_dir(state_dir).map_err(fail)?;
    write_record_atomic(state_dir, &path, record).map_err(fail)
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

/// The directory whose entry must be fsynced to make `path`'s own entry
/// durable — i.e. `path`'s parent, with one correction.
///
/// `Path::parent()` answers the **empty** path for a one-component relative
/// path: `Path::new("withdraw-state").parent()` is `Some("")`, not
/// `Some(".")`. `File::open("")` is `ENOENT`, so handing that straight to
/// the fsync refused the very first reservation for
/// `--state-dir withdraw-state` — before any burn, but for a perfectly
/// ordinary invocation.
///
/// The shipped profiles say `./withdraw-state`, whose parent IS `.`, which
/// is why nothing caught this: the bug needs a bare relative name, which is
/// exactly what an operator types when they are not copying from a profile.
///
/// `None` only for a path with no parent at all (`/`), where there is
/// nothing above to sync.
fn entry_parent(path: &Path) -> Option<&Path> {
    match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Some(Path::new(".")),
        other => other,
    }
}

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
/// Three steps, and which of them run on every call is the point, so they
/// are three functions rather than three blocks inside one `if`.
///
/// Only what THIS call creates is chmod'ed: an existing directory the
/// operator chose keeps whatever mode they gave it. That is a real
/// requirement, and it is also what made a half-finished setup permanent.
/// A first call that created the directory and then failed at the fsync or
/// the chmod refused the run — correctly — and every call after it found
/// the directory present and returned `Ok` from inside the existence guard
/// without ever finishing the job.
///
/// The two abandoned halves are not equally recoverable, and pretending
/// otherwise is how the guard came to cover both:
///
///  * **Durability** leaves no trace when it is skipped, and redoing it costs
///    one `open` and one `fsync`. So [`sync_entry_of`] runs unconditionally. It
///    covers the level the records live in; a deeper level whose sync was
///    abandoned is not covered, because nothing says it happened.
///  * **Mode** is observable but not attributable: 0755 here may be a directory
///    we failed to restrict or one an operator chose.
///    [`report_permissive_mode`] says what it sees instead of guessing — a
///    record carries no key material, but it does carry amounts and destination
///    addresses.
pub(crate) fn ensure_state_dir(state_dir: &Path) -> CliResult<()> {
    create_missing_levels(state_dir)?;
    sync_entry_of(state_dir)?;
    report_permissive_mode(state_dir);
    Ok(())
}

/// Create the levels that do not exist yet, sync each new level's entry,
/// and restrict the innermost one to 0700 — all of it only when there is
/// something to create.
fn create_missing_levels(state_dir: &Path) -> CliResult<()> {
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
                // The EMPTY path, which is what `parent()` answers for a
                // one-component relative path: `Path::new("withdraw-state")
                // .parent()` is `Some("")`, not `Some(".")`. Its parent is
                // the process's current directory, which always exists, so
                // there is nothing above this level for us to create. Stop
                // — walking into `""` would push it onto `created` as a
                // level we "made".
                Some(p) if p.as_os_str().is_empty() => break,
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
            let Some(parent) = entry_parent(level) else {
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

/// Make `path`'s own directory entry durable.
///
/// NOT best-effort, and not conditional. This runs before the reservation,
/// which runs before an irreversible burn: an entry we cannot make durable
/// is a reservation we cannot promise to find again, and a run that
/// created the directory and died before syncing it leaves nothing behind
/// for a later run to notice.
fn sync_entry_of(path: &Path) -> CliResult<()> {
    let Some(parent) = entry_parent(path) else {
        return Ok(());
    };
    std::fs::File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "idempotency: could not make {} durable (fsync of {}): {e}",
                path.display(),
                parent.display(),
            ),
            source: None,
        })
}

/// Is this mode readable, writable or traversable by anyone but the owner?
fn mode_is_permissive(mode: u32) -> bool {
    mode & 0o077 != 0
}

/// Report — never correct — a state directory anyone but its owner can
/// reach.
///
/// Correcting it would override an operator who meant it, and nothing on
/// disk distinguishes that from a run that created the directory and could
/// not restrict it. Saying what is there costs nothing and is the half
/// that was being lost silently.
fn report_permissive_mode(state_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = fs::metadata(state_dir) else {
        return;
    };
    let mode = md.permissions().mode() & 0o777;
    if mode_is_permissive(mode) {
        warn!(
            state_dir = %state_dir.display(),
            mode = format!("{mode:04o}"),
            "the withdrawal state directory can be reached by more than its owner. Records carry \
             no key material, but they do carry amounts and destination addresses. A run that \
             created this directory and could not restrict it to 0700 refuses — so a mode like \
             this one outlives that refusal, and `chmod 700` is the fix if it was not deliberate",
        );
    }
}

/// `pub(crate)` so a post-burn failure can name the exact file an operator
/// has to edit to resume. The directory holds one record per withdrawal
/// identity, named by a SHA-256 nobody can compute by hand — "the state
/// file" without a path is not a recovery instruction.
pub(crate) fn record_path(state_dir: &Path, key: &str) -> PathBuf {
    state_dir.join(format!("{key}.json"))
}

/// Held for as long as this process owns a withdrawal identity: across the
/// reservation, the burn, and the write that records its hash.
///
/// **It exists to answer one question the record cannot.** A record that
/// says `reserved` with no `an_tx_hash` has two readings — another run is
/// inside `burn::send` right now and has not come back to write the hash,
/// or a run died in that window and left it. `decide_burn` refuses both,
/// correctly, because the field that would tell them apart is written only
/// after the send returns. But the refusal then has nothing to tell the
/// operator except "reconcile", and the only escape they can find on their
/// own is deleting the record — which is the guard against a second burn.
///
/// `flock` answers it. The kernel releases the lock when the holding
/// process exits, however it exits, so "is somebody executing this
/// withdrawal right now" becomes a fact rather than a judgement about
/// wall-clock age or a `pgrep` that is wrong on a different user, under a
/// wrapper name, or racing.
///
/// **Advisory, and additional to the record guard — never instead of it.**
/// `flock` is per-host and is a no-op or an error on some network
/// filesystems, so a state dir shared over NFS between two machines is
/// exactly the case it cannot see. The cross-field checks in
/// [`read_record`] and the provenance check in `decide_burn` are what hold
/// there; this only makes the refusal actionable when it can.
///
/// Acquired BEFORE the reservation on purpose. Everything between the
/// reserve and the send is supposed to be infallible, and a lock taken
/// there would be a new way to fail with the identity already claimed.
/// Taken first, its only failure is a pre-send refusal like any other.
///
/// `#[must_use]`, though not for the reason it is usually reached for:
/// this type is only ever returned inside a `Result`, which already
/// warns. What it buys is the day somebody returns one bare. It does NOT
/// make the guard the compiler's business — `let _ = ..` silences
/// `must_use` and is exactly the form that would drop the lock on the
/// spot, which is why the binding in `run` is a named `_withdrawal_lock`
/// rather than a `_`.
#[must_use]
#[derive(Debug)]
pub struct WithdrawalLock {
    /// Dropping this releases the lock; so does the process dying.
    _file: fs::File,
}

/// What one attempt to take a withdrawal lock found.
///
/// Three outcomes rather than two-and-an-error, because the caller has to
/// treat three cases differently and the old shape could only express two.
/// `try_acquire` returned `Err` both for a filesystem that does not
/// implement `flock` and for a lock it simply failed to open, and the
/// caller read every `Err` as the first — "proceed, the record's own
/// guards hold here".
///
/// That is not a cosmetic conflation. A run that proceeds without the lock
/// is invisible to the liveness probe, so a second run is told "the record
/// was left by a run that has already exited" — which the runbook gives as
/// the condition for deleting the record. Deleting it while the first run
/// is inside `burn::send` is the second burn the lock exists to prevent.
/// The failures that matter are the ones that hit ONE process and not the
/// other: `EMFILE` is per-process, and a state directory left half-prepared
/// by a run that died mid-setup is finished for everyone but the run that
/// hit it.
#[must_use]
#[derive(Debug)]
pub enum LockAttempt {
    /// This process owns the identity until the guard is dropped.
    Held(WithdrawalLock),
    /// Another live process on this host owns it right now.
    Contended,
    /// This filesystem does not implement `flock`. A supported
    /// deployment — a network mount, typically — so not a refusal: the
    /// record's cross-field guards are what hold there, and any refusal
    /// says the evidence is missing rather than inventing it.
    Unsupported { why: String },
}

impl LockAttempt {
    /// What this attempt says about OTHER processes: `Some(true)` somebody
    /// holds the withdrawal, `Some(false)` nobody does, `None` no evidence.
    ///
    /// `Unsupported` is `None` and never `Some(false)`. That distinction is
    /// the whole point: `Some(false)` is what the runbook turns into
    /// permission to delete the record.
    #[must_use]
    pub fn holder_verdict(&self) -> Option<bool> {
        match self {
            LockAttempt::Held(_) => Some(false),
            LockAttempt::Contended => Some(true),
            LockAttempt::Unsupported {
                ..
            } => None,
        }
    }
}

/// How to read the errno `flock` reported.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FlockVerdict {
    /// Somebody holds it.
    Contended,
    /// The call cannot work on this filesystem, whoever runs it.
    Unsupported,
    /// Everything else. The caller must refuse rather than continue
    /// unlocked.
    Fatal,
}

/// An ALLOWLIST, and deliberately a short one.
///
/// The defect this replaces was a catch-all reading every unexpected errno
/// as "no `flock` here". Renaming a catch-all changes nothing, so the rule
/// is inverted: three errnos mean the filesystem cannot do this at all, and
/// everything else — `EACCES`, `EMFILE`, `EBADF`, `EINTR`, an errno this
/// build has never seen — is fatal. Being wrong in this direction costs a
/// refusal with nothing sent; being wrong in the other direction costs a
/// second burn.
fn classify_flock_error(raw: Option<i32>) -> FlockVerdict {
    match raw {
        // A guard rather than two patterns: the constants are equal on
        // Linux, so the second would be unreachable, and both are named
        // because POSIX lets them differ.
        Some(n) if n == libc::EWOULDBLOCK || n == libc::EAGAIN => FlockVerdict::Contended,
        // `ENOLCK` — no locks available, which is how several network
        // filesystems answer. `EOPNOTSUPP` and `ENOSYS` — the operation
        // is not implemented. Nothing about the caller, everything about
        // the filesystem, and identical for every process on the host.
        Some(n) if n == libc::ENOLCK || n == libc::EOPNOTSUPP || n == libc::ENOSYS => {
            FlockVerdict::Unsupported
        },
        _ => FlockVerdict::Fatal,
    }
}

impl WithdrawalLock {
    fn path(state_dir: &Path, key: &str) -> PathBuf {
        state_dir.join(format!("{key}.lock"))
    }

    /// Take the lock, or say what stopped us.
    ///
    /// `Err` is a refusal the caller must propagate — it means this
    /// process could not attempt the lock, which is not the same as the
    /// filesystem being unable to. [`LockAttempt`] carries that
    /// distinction; it used to be lost, and losing it ends in a second
    /// burn.
    ///
    /// **The state directory must already exist.** Creating it here was
    /// the fix for one instance of exactly the bug above — the open
    /// returned `ENOENT` on a first invocation and the caller read it as
    /// "no flock here" — but it left directory preparation able to
    /// masquerade as a verdict about locking. `reserve_and_decide` calls
    /// `ensure_state_dir` before this, so a directory that cannot be
    /// prepared is a refusal about the directory, said in those words.
    pub fn try_acquire(state_dir: &Path, key: &str) -> CliResult<LockAttempt> {
        use std::os::unix::io::AsRawFd;

        let path = Self::path(state_dir, key);
        let refuse = |what: &str, e: std::io::Error| CliError::Preflight {
            reason: format!(
                "idempotency: {what} {}: {e} (nothing was sent)",
                path.display()
            ),
            source: None,
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| refuse("open withdrawal lock", e))?;

        // SAFETY: `file` outlives the call, so the descriptor is valid.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(LockAttempt::Held(Self {
                _file: file,
            }));
        }
        let e = std::io::Error::last_os_error();
        match classify_flock_error(e.raw_os_error()) {
            FlockVerdict::Contended => Ok(LockAttempt::Contended),
            FlockVerdict::Unsupported => Ok(LockAttempt::Unsupported {
                why: e.to_string(),
            }),
            FlockVerdict::Fatal => Err(refuse("lock", e)),
        }
    }

    /// Whether a live process on this host is executing `key` right now.
    ///
    /// The question the record cannot answer, asked by a run that does
    /// NOT hold the lock — a refusal deciding what to tell the operator.
    /// Takes the lock and drops it immediately, so asking never keeps
    /// anybody out.
    ///
    /// `None` when `flock` could not be attempted at all, which is not
    /// the same as "nobody holds it" and must not be reported as if it
    /// were: a state dir on a filesystem without working `flock` is a
    /// supported deployment.
    ///
    /// (This existed, was unused once the acquisition itself started
    /// carrying the answer, and was removed. It is back because the
    /// stage-1 refusal — the one an operator reaches by re-running the
    /// identical command, which is what the recovery procedure tells
    /// them to do — has no acquisition to learn from.)
    #[must_use]
    pub fn probe_holder(state_dir: &Path, key: &str) -> Option<bool> {
        // `Held` is dropped at the end of this expression, which is the
        // point: asking must not keep anybody out.
        //
        // `Err` collapses to `None` — no evidence — rather than to a
        // verdict. This function is called while BUILDING a refusal, so it
        // has nowhere to propagate to; what it must never do is answer
        // "nobody holds it" because it could not ask.
        Self::try_acquire(state_dir, key)
            .ok()
            .and_then(|a| a.holder_verdict())
    }
}

/// The sentence a refusal prints about who is executing this withdrawal.
///
/// Takes what the lock could tell us — `Some(true)` somebody holds it,
/// `Some(false)` nobody does, `None` we could not ask — and returns the
/// verdict for it. One function because three refusals print this and they
/// have to agree: it is the line that decides whether an operator deletes
/// a record, and deleting one while a burn is mid-send is the second burn
/// the whole guard exists to prevent.
#[must_use]
pub fn liveness_verdict(holder: Option<bool>) -> &'static str {
    match holder {
        Some(true) => {
            "Another process on this host is executing this withdrawal RIGHT NOW (it holds the \
             withdrawal lock). Do not touch the record and do not delete it: wait for that run to \
             finish and read its outcome. Nothing was sent by this run."
        },
        Some(false) => {
            "No other process on this host holds this withdrawal, so the record was left by a run \
             that has already exited. That is not the same as \"nothing was broadcast\": a run can \
             exit between the send returning and the hash being written, which is exactly the \
             record you are looking at."
        },
        None => {
            "Whether another process holds this withdrawal could not be determined here (`flock` \
             is unavailable — a network mount, typically), so the on-chain reconciliation below is \
             the only evidence available."
        },
    }
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
        eth_tx_hash: None,
    }
}

fn read_record(path: &Path) -> CliResult<Record> {
    let bytes = fs::read(path).map_err(|e| CliError::Preflight {
        reason: format!("idempotency: read {}: {e}", path.display()),
        source: None,
    })?;
    let record: Record = serde_json::from_slice(&bytes).map_err(|e| CliError::Preflight {
        reason: format!(
            "idempotency: prior record at {} is corrupt: {e} (delete it manually if you know it's \
             stale)",
            path.display()
        ),
        source: None,
    })?;

    // Serde only checks that the FIELDS are well typed. Two cross-field
    // invariants are what actually make this record safe to act on, and
    // both are reachable by hand — the CLI's own post-burn recovery message
    // asks an operator to edit `an_tx_hash` and `status` by hand, so a
    // fumbled edit is an expected input, not an exotic one.
    //
    // 1. A status at or past `Burned` without a hash. `{"status":"burned",
    //    "an_tx_hash":null}` deserialises cleanly, and every consumer then reads
    //    "no hash" as "nothing was sent" — so the run burns again, against a record
    //    that says a burn already happened.
    //
    // `Failed` belongs in this list and was missing from it. Its ONLY
    // production writer is the `withdrawByProof` revert path
    // (`orchestrator.rs`), which by construction runs after a successful
    // burn — so a `Failed` record without a hash is exactly as impossible
    // as a `Burned` one, and exactly as hand-editable. Leaving it out is
    // what made `reserve`'s wipe branch reachable, and that branch
    // published a fresh record with a plain rename and called the result
    // `Created` — the one word that is supposed to mean "this run won the
    // atomic publish". Two runs could both win it.
    if record.an_tx_hash.is_none()
        && matches!(
            record.status,
            Status::Burned
                | Status::Captured
                | Status::Proved
                | Status::Submitted
                | Status::Confirmed
                | Status::Failed
        )
    {
        return Err(CliError::Preflight {
            reason: format!(
                "idempotency: prior record at {} says status={:?} but carries no an_tx_hash. \
                 Those cannot both be true: every status past `reserved` is written only after a \
                 burn was broadcast, and the hash is written with it.\n\x20 This is what a \
                 hand-edited record looks like when the status was set and the hash was not. \
                 Acting on it would broadcast a SECOND burn.\n\x20 Reconcile on chain (runbook \
                 Case 3a), then either fill in an_tx_hash or set status back to \"reserved\".",
                path.display(),
                record.status,
            ),
            source: None,
        });
    }

    // 2. The record's own key must match the filename it was read from. The name IS
    //    the identity — a SHA-256 over (from, to, chain, amount) — so a record
    //    copied or renamed into another identity's slot would let one withdrawal's
    //    burn vouch for a different one.
    let expected = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if record.key != expected {
        return Err(CliError::Preflight {
            reason: format!(
                "idempotency: prior record at {} carries key {} but its filename says {}. The \
                 filename is the withdrawal identity, so this record belongs to a different \
                 withdrawal — it was copied or renamed here. Refusing rather than letting one \
                 withdrawal's state answer for another.",
                path.display(),
                record.key,
                expected,
            ),
            source: None,
        });
    }

    Ok(record)
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
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use alloy_primitives::Address;
    use tempfile::TempDir;

    use super::*;

    /// `reserve` keeping only the record.
    ///
    /// Most tests here predate `Reservation` and are about the record's
    /// contents, not about which side of `create_new` produced it. The
    /// provenance has its own test below, so widening the return type did
    /// not quietly stop anything from being checked.
    fn reserve_rec(
        state_dir: &Path,
        from: &FromAddress,
        to: &ToAddress,
        amount: &UsdcAmount,
        allow_retry: bool,
    ) -> CliResult<Record> {
        reserve(state_dir, from, to, amount, allow_retry).map(|(r, _)| r)
    }

    #[test]
    fn a_hand_edited_record_claiming_a_burn_without_a_hash_is_refused() {
        // The CLI's own post-burn recovery message asks the operator to
        // write an_tx_hash and set status to "burned" by hand. Setting the
        // status and fumbling the hash produces exactly this record — which
        // serde accepts, and which every consumer then reads as "no burn
        // happened", so the next run burns again.
        let dir = TempDir::new().unwrap();
        let k = key(&sample_from(), &sample_to(), &UsdcAmount(500_000));
        let mut rec = fresh_reserved_record(&k, &sample_from(), &sample_to(), &UsdcAmount(500_000));
        rec.status = Status::Burned;
        assert!(rec.an_tx_hash.is_none());
        std::fs::write(
            record_path(dir.path(), &k),
            serde_json::to_vec_pretty(&rec).unwrap(),
        )
        .unwrap();

        let err = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        )
        .expect_err("burned-without-a-hash is not a state the field can produce");
        let msg = format!("{err}");
        assert!(
            msg.contains("no an_tx_hash"),
            "must name the contradiction, got: {msg}",
        );
        assert!(
            msg.contains("SECOND burn"),
            "must say what acting on it would cost, got: {msg}",
        );
    }

    #[test]
    fn a_record_copied_into_another_identitys_slot_is_refused() {
        // The filename IS the identity — a SHA-256 over (from, to, chain,
        // amount). A record carrying a different key in that slot would let
        // one withdrawal's burn vouch for another's.
        let dir = TempDir::new().unwrap();
        let mine = key(&sample_from(), &sample_to(), &UsdcAmount(500_000));
        let theirs = key(&sample_from(), &sample_to(), &UsdcAmount(999_999));
        let rec =
            fresh_reserved_record(&theirs, &sample_from(), &sample_to(), &UsdcAmount(999_999));
        std::fs::write(
            record_path(dir.path(), &mine),
            serde_json::to_vec_pretty(&rec).unwrap(),
        )
        .unwrap();

        let err = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        )
        .expect_err("a record in the wrong slot must not answer for this withdrawal");
        assert!(
            format!("{err}").contains("belongs to a different withdrawal"),
            "got: {err}",
        );
    }

    #[test]
    fn reserve_reports_whether_it_created_the_record() {
        // The distinction the concurrent double-burn turned on. `Created`
        // means this run won `create_new` and owns the identity; `Found`
        // means someone else does — possibly a run that is inside
        // `burn::send` right now and has not written its hash yet.
        let dir = TempDir::new().unwrap();
        let (_, how) = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        assert_eq!(how, Reservation::Created, "first writer wins create_new");

        let (rec, how) = reserve(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        )
        .unwrap();
        assert_eq!(how, Reservation::Found, "second run reads what is there");
        assert!(
            rec.an_tx_hash.is_none(),
            "and it has no hash yet — which is exactly why the fields alone cannot tell this \
             apart from a fresh reservation",
        );
    }

    #[test]
    fn only_one_of_many_racing_reservations_creates() {
        // There were no concurrency tests on this store at all, which is
        // why the concurrent double-burn survived a round of review: every
        // existing test calls `reserve` sequentially, and sequentially the
        // second call always sees a record whose fields have settled.
        //
        // Eight threads, one identity, all with --allow-retry. `create_new`
        // is atomic, so exactly one must come back `Created`; the rest are
        // `Found` and, having no hash to reuse, must refuse rather than
        // send.
        use std::sync::Arc;

        let dir = Arc::new(TempDir::new().unwrap());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let dir = Arc::clone(&dir);
            handles.push(std::thread::spawn(move || {
                reserve(
                    dir.path(),
                    &sample_from(),
                    &sample_to(),
                    &UsdcAmount(500_000),
                    true,
                )
                .map(|(_, how)| how)
            }));
        }
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let created = outcomes
            .iter()
            .filter(|o| matches!(o, Ok(Reservation::Created)))
            .count();
        assert_eq!(
            created, 1,
            "exactly one racing run may own the identity, got {created}: {outcomes:?}",
        );
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, Ok(Reservation::Created) | Ok(Reservation::Found))),
            "with --allow-retry none of them should error outright: {outcomes:?}",
        );
    }

    #[test]
    fn the_lock_probe_reports_a_live_holder_as_a_live_holder() {
        // The verdict that authorises deleting a record, and until now
        // nothing reached it: two references in the tree, the definition
        // and one call site. Swapping its two arms compiles and passes
        // everything, and tells an operator whose withdrawal is mid-send
        // that the holder has already exited — two lines above the
        // refusal's offer to delete the record.
        let dir = TempDir::new().unwrap();
        let k = key(&sample_from(), &sample_to(), &UsdcAmount(1));

        assert_eq!(
            WithdrawalLock::probe_holder(dir.path(), &k),
            Some(false),
            "an untouched withdrawal has no holder",
        );

        let held = match WithdrawalLock::try_acquire(dir.path(), &k)
            .expect("flock works on this filesystem")
        {
            LockAttempt::Held(l) => l,
            other => panic!("nobody else holds it: {other:?}"),
        };
        assert_eq!(
            WithdrawalLock::probe_holder(dir.path(), &k),
            Some(true),
            "a held withdrawal has a holder",
        );
        // And asking does not take it away from the holder — the probe
        // acquires and drops, so a second ask must still say "held".
        assert_eq!(
            WithdrawalLock::probe_holder(dir.path(), &k),
            Some(true),
            "asking must not release somebody else's lock",
        );

        drop(held);
        assert_eq!(
            WithdrawalLock::probe_holder(dir.path(), &k),
            Some(false),
            "released means released",
        );
    }

    #[test]
    fn the_entry_sync_reports_failure_rather_than_shrugging() {
        // The half a first call can abandon without leaving a trace:
        // `create_dir_all` succeeds, this fails, the run refuses — and
        // before the split, every later call found the directory present
        // and returned `Ok` from inside the existence guard without ever
        // syncing. Whether it now runs on every call is visible in
        // `ensure_state_dir`'s three-line body; what a test can pin is
        // that it is not best-effort, because an `fsync` that happened is
        // not observable from here at all.
        let dir = TempDir::new().unwrap();
        let ok = dir.path().join("state");
        std::fs::create_dir(&ok).unwrap();
        sync_entry_of(&ok).expect("a real parent syncs");

        // A parent that is not there. `File::open` on a REGULAR file
        // succeeds — it is only an open — so a regular-file parent is not
        // the way to make this fail; an absent one is, and it is
        // uid-independent, unlike a chmod, which root ignores.
        let err = sync_entry_of(&dir.path().join("vanished").join("state"))
            .expect_err("a parent that does not exist cannot be synced");
        assert_eq!(err.exit_code().as_i32(), 2, "{err}");
        assert!(
            format!("{err}").contains("durable"),
            "must name what was lost: {err}",
        );
    }

    #[test]
    fn a_state_directory_reachable_by_anyone_else_is_reported() {
        // 0700 is what this tool creates; anything looser is either an
        // operator's choice or a run that could not restrict what it made,
        // and nothing on disk tells them apart. So: report, never correct.
        assert!(
            !mode_is_permissive(0o700),
            "what we create is not a finding"
        );
        assert!(!mode_is_permissive(0o500));
        assert!(mode_is_permissive(0o750), "group can traverse and read");
        assert!(mode_is_permissive(0o755));
        assert!(mode_is_permissive(0o701), "other can traverse");
        assert!(mode_is_permissive(0o770));
    }

    #[test]
    fn preparing_a_directory_that_already_exists_leaves_its_mode_alone() {
        // The requirement the existence guard is there for, kept: an
        // operator who chose a mode keeps it, and the report above is what
        // stops that being silent.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, fs::Permissions::from_mode(0o750)).unwrap();

        ensure_state_dir(&state).expect("an existing directory is usable");

        let mode = fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750, "a mode we did not set is not ours to change");
    }

    #[test]
    fn a_record_survives_a_rollback_across_the_dropped_proof_path_field() {
        // Removing a field from a persisted record has two directions,
        // and only one of them is obvious. A NEW build reading an OLD
        // record must ignore the field it no longer knows; an OLD build
        // reading a NEW record must not choke on its absence — which is
        // what a rollback in the middle of an in-flight withdrawal does.
        //
        // If it did choke, the message it would give is
        // "prior record is corrupt … delete it manually if you know it's
        // stale", and deleting a record mid-withdrawal is the one action
        // every guard in this module exists to prevent. That is why this
        // is asserted rather than assumed.
        let base = r#""key":"k","status":"burned","from_extended":"a::b","to_hex":"0x00",
            "to_chain":1,"amount_micro":1,"reserved_at":"now",
            "an_tx_hash":"0xab","withdrawal_msg_id":null,"block_seq_no":null,
            "eth_tx_hash":null"#;

        // 1. An old record, still carrying the field this build dropped.
        let old = format!(r#"{{{base},"proof_json_path":"/tmp/proof_event_000001.json"}}"#);
        let r: Record = serde_json::from_str(&old)
            .expect("a field this build no longer knows is ignored, not an error");
        assert_eq!(r.an_tx_hash.as_deref(), Some("0xab"));

        // 2. A new record, without it — what an older build would read after a
        //    rollback. `Option` is serde's one defaulting case, which is the whole
        //    reason this removal is safe.
        let new = format!("{{{base}}}");
        let r: Record = serde_json::from_str(&new).expect("a missing optional field is None");
        assert_eq!(r.status, Status::Burned);
    }

    #[test]
    fn only_three_errnos_mean_this_filesystem_cannot_lock() {
        // An allowlist, asserted as one. The defect being replaced was a
        // catch-all that read every unexpected errno as "no flock here"
        // and carried on unlocked; renaming a catch-all would have changed
        // nothing, so what has to be pinned is that the default is fatal.
        //
        // `EMFILE` is the one to look at twice: it is per-process, so the
        // run that hits it proceeds unlocked while every other process on
        // the host opens the same lock normally — and is then told that
        // this run has already exited.
        assert_eq!(
            classify_flock_error(Some(libc::EWOULDBLOCK)),
            FlockVerdict::Contended,
        );
        assert_eq!(
            classify_flock_error(Some(libc::EAGAIN)),
            FlockVerdict::Contended,
        );

        for unsupported in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOSYS] {
            assert_eq!(
                classify_flock_error(Some(unsupported)),
                FlockVerdict::Unsupported,
                "errno {unsupported} is a property of the filesystem, not of this run",
            );
        }

        for fatal in [
            libc::EACCES,
            libc::EMFILE,
            libc::ENFILE,
            libc::EBADF,
            libc::EINTR,
            libc::ENOSPC,
            libc::EROFS,
            libc::EIO,
            // An errno this build has never seen. The default has to be
            // fatal, or the next unfamiliar one repeats the defect.
            424_242,
        ] {
            assert_eq!(
                classify_flock_error(Some(fatal)),
                FlockVerdict::Fatal,
                "errno {fatal} says nothing about whether this filesystem can lock, so it may not \
                 be read as permission to continue unlocked",
            );
        }

        // No errno at all is not evidence of anything either.
        assert_eq!(classify_flock_error(None), FlockVerdict::Fatal);
    }

    #[test]
    fn no_attempt_that_failed_to_ask_reports_the_withdrawal_free() {
        // `Some(false)` is the verdict the runbook turns into permission
        // to delete the record. Only an attempt that actually took the
        // lock may produce it.
        assert_eq!(
            LockAttempt::Unsupported {
                why: "whatever the mount said".into(),
            }
            .holder_verdict(),
            None,
            "\"we cannot ask here\" is not \"nobody holds it\"",
        );
        assert_eq!(LockAttempt::Contended.holder_verdict(), Some(true));
    }

    #[test]
    fn a_lock_that_cannot_be_attempted_is_not_reported_as_free() {
        // `None`, never `Some(false)`. A state dir on a filesystem
        // without working `flock` is a supported deployment, and
        // answering "nobody holds it" there is the same false clearance
        // to delete a record that may be a burn in flight.
        let dir = TempDir::new().unwrap();
        // A parent that is a regular file: ENOTDIR for every uid, unlike
        // a chmod, which root ignores.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        assert_eq!(
            WithdrawalLock::probe_holder(&blocker, "0123456789abcdef"),
            None,
            "a lock that could not be attempted is not a lock that is free",
        );
    }

    #[test]
    fn the_liveness_verdict_says_wait_only_when_somebody_is_holding_it() {
        let held = liveness_verdict(Some(true));
        let free = liveness_verdict(Some(false));
        let unknown = liveness_verdict(None);

        assert!(held.contains("RIGHT NOW"), "{held}");
        assert!(held.contains("do not delete it"), "{held}");

        assert!(free.contains("has already exited"), "{free}");
        assert!(!free.contains("RIGHT NOW"), "{free}");
        // Free is not a clearance either: the record can still be a burn
        // whose hash was never written, and the sentence has to say so.
        assert!(free.contains("not the same as"), "{free}");

        assert!(unknown.contains("could not be determined"), "{unknown}");
        assert!(!unknown.contains("RIGHT NOW"), "{unknown}");
        assert!(!unknown.contains("has already exited"), "{unknown}");
    }

    #[test]
    fn only_confirmed_and_submitted_are_terminal_everywhere_that_asks() {
        // Three places have to agree and two of them are match patterns,
        // which no compiler compares: `Status::is_terminal`, the arm in
        // `reserve` that builds the refusal behind an `expect`, and the
        // resume path, which asks by status after restoring a record that
        // was deleted mid-preflight. Drift between them is a paid-out
        // withdrawal carried through a second `withdrawByProof`.
        //
        // The list is written out rather than derived, so changing
        // `is_terminal` fails here instead of quietly agreeing with
        // itself.
        for (status, terminal) in [
            (Status::Reserved, false),
            (Status::Burned, false),
            (Status::Captured, false),
            (Status::Proved, false),
            (Status::Failed, false),
            (Status::Submitted, true),
            (Status::Confirmed, true),
        ] {
            assert_eq!(status.is_terminal(), terminal, "{status:?}");

            // And `reserve`'s own arm, driven against a real state
            // directory. Every planted record carries a hash, so
            // `--allow-retry` resumes each non-terminal one and the only
            // refusals left are the terminal pair.
            let dir = TempDir::new().unwrap();
            let mut rec = fresh_reserved_record(
                &key(&sample_from(), &sample_to(), &UsdcAmount(1)),
                &sample_from(),
                &sample_to(),
                &UsdcAmount(1),
            );
            rec.status = status;
            rec.an_tx_hash = Some(format!("0x{}", "ab".repeat(32)));
            update(dir.path(), &rec).unwrap();

            let got = reserve(
                dir.path(),
                &sample_from(),
                &sample_to(),
                &UsdcAmount(1),
                true,
            );
            assert_eq!(
                got.is_err(),
                terminal,
                "{status:?}: reserve's arm and is_terminal must name the same statuses",
            );
            if let Err(e) = got {
                assert_eq!(e.exit_code().as_i32(), 3, "{status:?}: {e}");
            }
        }

        let mut rec = fresh_reserved_record(
            &key(&sample_from(), &sample_to(), &UsdcAmount(1)),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(1),
        );

        // And the two that are terminal say different things, because the
        // operator's next move differs.
        rec.status = Status::Confirmed;
        let paid = format!("{}", terminal_refusal(&rec).expect("confirmed is terminal"));
        rec.status = Status::Submitted;
        let unresolved = format!("{}", terminal_refusal(&rec).expect("submitted is terminal"));
        assert!(paid.contains("already paid out"), "{paid}");
        assert!(
            unresolved.contains("receipt was never observed"),
            "{unresolved}"
        );
        for m in [&paid, &unresolved] {
            assert!(
                !m.contains("re-run with --allow-retry"),
                "the one flag that changes nothing about a terminal record: {m}",
            );
        }
    }

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
        let rec = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .expect("first reserve should succeed");
        assert_eq!(rec.status, Status::Reserved);
        assert!(rec.an_tx_hash.is_none());
        assert!(dir.path().join(format!("{}.json", rec.key)).exists());
    }

    #[test]
    fn reserve_duplicate_active_refuses() {
        // A prior record that CARRIES A HASH and no `--allow-retry`: the
        // flag really is the answer here, so naming it is right.
        let dir = TempDir::new().unwrap();
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        first.status = Status::Burned;
        first.an_tx_hash = Some(format!("0x{}", "ab".repeat(32)));
        update(dir.path(), &first).unwrap();

        let second = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        );
        match second {
            Err(CliError::DuplicateInFlight {
                prior_status,
                prior_tx,
                ..
            }) => {
                assert_eq!(prior_status, "burned");
                assert!(
                    prior_tx.is_some(),
                    "the flag advice needs a hash to be about"
                );
            },
            other => panic!("expected DuplicateInFlight, got {other:?}"),
        }
    }

    #[test]
    fn reserve_allows_retry_over_active_when_flag_set() {
        let dir = TempDir::new().unwrap();
        let _first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        let second = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        )
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
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        first.status = Status::Burned;
        first.an_tx_hash = Some("0xdeadbeef".into());
        update(dir.path(), &first).unwrap();

        let resumed = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        )
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
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        // Same as above: a confirmed payout implies a burn.
        first.status = Status::Confirmed;
        first.an_tx_hash = Some("0xburned".into());
        first.eth_tx_hash = Some("0xabc".into());
        update(dir.path(), &first).unwrap();

        let attempt = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        );
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
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        // The AN burn necessarily happened first — `Submitted` cannot
        // exist without it, and `read_record` now enforces that — so the
        // fixture has to carry the hash to be a record the field could
        // actually produce.
        first.status = Status::Submitted;
        first.an_tx_hash = Some("0xburned".into());
        first.eth_tx_hash = Some("0xpending".into());
        update(dir.path(), &first).unwrap();

        let attempt = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            true,
        );
        assert!(
            matches!(attempt, Err(CliError::DuplicateInFlight { .. })),
            "Submitted must refuse until reconciled, got {attempt:?}"
        );
    }

    #[test]
    fn a_failed_record_with_no_hash_is_refused_not_wiped() {
        // This test used to assert the opposite, and the behaviour it
        // asserted was a double-burn path.
        //
        // `reserve` wiped a `Failed`/no-hash record back to a fresh
        // `Reserved` one and reported `Reservation::Created`. That word is
        // the caller's evidence that this run won the atomic publish — but
        // the wipe went through `rename`, which excludes nobody. Two runs
        // reading the same record both wiped, both were told they created
        // it, and `decide_burn` sent for both.
        //
        // The justification in the comment ("only ever written by a
        // pre-burn path") described a writer that does not exist: the sole
        // production writer of `Failed` is the post-burn `withdrawByProof`
        // revert. So the record is a hand-edit, and `read_record` now
        // refuses it with the same message as `burned`-with-no-hash.
        let dir = TempDir::new().unwrap();
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        first.status = Status::Failed;
        // an_tx_hash intentionally left None — the impossible combination.
        update(dir.path(), &first).unwrap();

        let err = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .expect_err("a Failed record with no hash cannot be acted on");
        let msg = format!("{err}");
        assert!(
            msg.contains("no an_tx_hash"),
            "must name the contradiction: {msg}"
        );
        assert!(
            msg.contains("SECOND burn"),
            "must say what acting on it would cost: {msg}"
        );

        // And `peek` refuses it too, so a real run stops at stage 1 rather
        // than after the confirmation prompt and `compose`.
        assert!(
            peek(
                dir.path(),
                &sample_from(),
                &sample_to(),
                &UsdcAmount(500_000)
            )
            .is_err(),
            "the same record must not read as absent"
        );
    }

    #[test]
    fn every_created_reservation_came_from_the_atomic_publish() {
        // The invariant the wipe broke, stated where a reader can check
        // it: `Reservation::Created` is returned from exactly one place,
        // the `hard_link` that fails EEXIST when somebody else won.
        //
        // Not a behavioural test — a grep with an assertion on it. If a
        // future branch grows a second `Created`, this fails and whoever
        // wrote it has to argue that their publish excludes too.
        let src = include_str!("idempotency.rs");
        // The needle is built from two pieces on purpose: spelled whole,
        // it would appear in this file and count itself.
        let needle = concat!("Ok((record, Reservation::", "Created))");
        assert_eq!(
            src.matches(needle).count(),
            1,
            "exactly one site may report a created reservation",
        );
        assert!(
            src.contains(concat!("std::fs::hard_", "link(tmp.path(), &path)")),
            "and that site must still be the one that publishes by hard_link",
        );
    }

    #[test]
    fn reserve_over_failed_with_an_tx_hash_preserves_prior_record() {
        // Regression (Sergey review 2026-09-01, «Failed всё ещё жжёт ECC
        // второй раз»): the sole production writer of Status::Failed is
        // the withdrawByProof-revert arm of the orchestrator, which only
        // fires after a successful AN burn. If reserve_rec() wiped the
        // record on Failed, the stored `an_tx_hash` would be dropped and
        // the orchestrator's Some(existing) resume branch would miss —
        // firing a SECOND `initiateWithdrawal` on retry (double-burn of
        // ECC[3] on the AN side). Retry after Case 3e (top-up-treasury,
        // re-run) must resume without --allow-retry.
        let dir = TempDir::new().unwrap();
        let mut first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        first.status = Status::Failed;
        first.an_tx_hash = Some("0xdeadbeef".into());
        first.withdrawal_msg_id = Some("0xmsg".into());
        first.block_seq_no = Some(12345);
        update(dir.path(), &first).unwrap();

        let second = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .expect("Failed with an_tx_hash must resume without --allow-retry");
        assert_eq!(
            second.status,
            Status::Failed,
            "prior status preserved for resume"
        );
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
        let mut rec = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
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
    fn a_failed_state_write_after_the_wire_is_never_exit_2() {
        use crate::errors::ExitCode;

        // Exit 2 is published as "refused before sending, nothing left the
        // machine". Four call sites reached it with `?` after the burn — two
        // of them after withdrawByProof had already paid out — because
        // `update` returned a plain `CliError` and the compiler had no
        // opinion. `UpdateFailed` has no `From<..> for CliError`, so `?` no
        // longer compiles and the code must be chosen; these assert that
        // both choices map where they claim.
        let dir = TempDir::new().unwrap();
        let rec = fresh_reserved_record(
            &key(&sample_from(), &sample_to(), &UsdcAmount(1)),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(1),
        );
        // A path that cannot be written: its parent is a regular file, which
        // is ENOTDIR for every uid — unlike a chmod, which root ignores.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let e = update(&blocker.join("state"), &rec)
            .expect_err("writing under a regular file must fail");

        assert_eq!(
            e.after_send("the AN burn is on the wire").exit_code(),
            ExitCode::BurnOutcomeUnknown,
            "once anything is on the wire the honest code is 10, never 2",
        );

        let e = update(&blocker.join("state"), &rec).expect_err("same failure");
        let before = e.before_send();
        assert_eq!(before.exit_code(), ExitCode::PreflightRefused);
        assert!(
            format!("{before}").contains("nothing was sent"),
            "the pre-send arm must still say so: {before}",
        );
    }

    #[test]
    fn a_bare_relative_state_dir_has_a_syncable_parent() {
        // `--state-dir withdraw-state` — no `./`, which is what an operator
        // types when not copying from a profile. `Path::parent()` answers
        // the EMPTY path there, `File::open("")` is ENOENT, and the
        // durability fsync (which is deliberately NOT best-effort) then
        // refused the first reservation outright.
        //
        // The two facts the fix rests on, asserted rather than assumed:
        assert_eq!(
            Path::new("withdraw-state").parent(),
            Some(Path::new("")),
            "a one-component relative path's parent is empty, not `.`",
        );
        assert!(
            !Path::new("").exists(),
            "the empty path does not exist, which is what made the walk-up loop push it and the \
             fsync open it",
        );

        assert_eq!(
            entry_parent(Path::new("withdraw-state")),
            Some(Path::new(".")),
            "the empty parent must be resolved to the current directory",
        );
        assert!(
            std::fs::File::open(entry_parent(Path::new("withdraw-state")).unwrap()).is_ok(),
            "and that directory must actually be openable for fsync",
        );

        // Unchanged for every other shape, including the one the shipped
        // profiles use.
        assert_eq!(
            entry_parent(Path::new("./withdraw-state")),
            Some(Path::new(".")),
        );
        assert_eq!(
            entry_parent(Path::new("/var/lib/bridge/state")),
            Some(Path::new("/var/lib/bridge")),
        );
        assert_eq!(
            entry_parent(Path::new("/")),
            None,
            "the root has no parent to sync",
        );
    }

    #[test]
    fn a_hash_less_prior_is_never_reported_as_a_created_reservation() {
        // The double-spend guard, stated as the thing this function is
        // actually responsible for. `Reserved` + no hash is also what a
        // burn that reached the wire and errored leaves behind, so it must
        // never look like an identity this run just claimed.
        //
        // The refusal itself is `decide_burn`'s, not this function's, and
        // deliberately so: it is the caller that knows whether this run
        // holds the withdrawal lock, and that is the half of the answer
        // the operator needs before deleting anything. What this function
        // owes is the provenance — `Found`, never `Created` — which is
        // exactly the input `decide_burn` refuses on. Refusing here as
        // well only bought a second message, and the second message named
        // `--allow-retry`, which the refusal downstream then says does not
        // override it.
        let dir = TempDir::new().unwrap();
        let first = reserve_rec(
            dir.path(),
            &sample_from(),
            &sample_to(),
            &UsdcAmount(500_000),
            false,
        )
        .unwrap();
        assert!(first.an_tx_hash.is_none());

        for allow_retry in [false, true] {
            let (rec, how) = reserve(
                dir.path(),
                &sample_from(),
                &sample_to(),
                &UsdcAmount(500_000),
                allow_retry,
            )
            .expect("the record is handed back for the caller to judge");
            assert!(rec.an_tx_hash.is_none());
            assert_eq!(
                how,
                Reservation::Found,
                "--allow-retry={allow_retry}: a record somebody else published is never this \
                 run's to burn against",
            );
        }
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

        let reserved = reserve_rec(
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

        let mut rec = reserve_rec(
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
        let mut rec = reserve_rec(
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
        let mut rec = reserve_rec(
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

        reserve_rec(
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

        let err = reserve_rec(
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
        let err = reserve_rec(
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
