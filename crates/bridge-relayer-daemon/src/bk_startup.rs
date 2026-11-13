//! The BK set `daemon-live` starts the prover with.
//!
//! `state/prover_bk_set.json` is the first source. It is taken as it is when
//! its commitment is the one the mirror expects, its keys hash to that
//! commitment and its rotation cursor is the mirror's. When only the cursor is
//! off, the cursor is fixed in place. Otherwise the set is rebuilt from
//! GraphQL at the height [`bk_target`] picks and checked against the
//! commitment the mirror expects before anything is written.
//!
//! The pair of state files is written set first, then mirror
//! ([`crate::live_source::persist_bk_pair`]). Recovery does not rest on that
//! order: a write cut short anywhere leaves a consistent pair, a set with the
//! expected keys under another cursor, or a set with another commitment, and
//! [`classify_bk_file`] turns these into [`BkFileAction::Keep`],
//! [`BkFileAction::FixCursor`] and [`BkFileAction::Rebuild`].
//!
//! [`startup_bk`] is the entry point: it takes the startup decision, picks
//! the mirror the driver starts with and the set that mirror needs, and
//! writes what changed. [`startup_bk_or_shutdown`] runs it until a shutdown
//! signal.

use std::{
    ffi::OsString,
    future::Future,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use bridge_gql_fetcher::gql_client::UnknownFieldError;
pub use bridge_prover_lib::bk_set_history::bk_target;
use bridge_prover_lib::{
    bk_set_history::{build_bk_set_with_retry, BkSetHistory, BuildError, BuildPolicy},
    bk_set_replay::{commitment, BkTarget, FinalCheck},
    bridge_state::{BridgeState, EthBridgeContractState},
    live_driver::{SeedBk, SeedPolicy},
    prover_bk_set::ProverBkSet,
};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use tracing::{info, warn};

use crate::{
    live_source::{persist_bk_pair, StatePaths},
    startup_decide::StartupDecision,
};

/// The BK fields a mirror takes from the contract, and the height the
/// prover's set is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractBk {
    /// Commitment of the set the prover must hold.
    pub commitment: [u8; 32],
    /// The mirror's rotation cursor.
    pub last_bk_set_update_seq_no: u64,
    pub target: BkTarget,
}

/// BK fields of a mirror built from the contract (Resurrect, a resync of the
/// BK fields, Cold): L is `storedLastSeenBlockSeqNo`, N
/// `storedLastBkSetUpdateSeqNo`, and the target is `bk_target(L, N, stride)`.
///
/// Outside the hold window the mirror takes `storedBkSetCommitment` and N. In
/// the hold window the contract has applied N while bundles at or below N,
/// signed by the outgoing set, are still owed. The mirror then takes
/// `storedPrevBkSetCommitment` and, as its cursor, the newest rotation below
/// N (0 when there is none), so that the driver takes N next as a rotation
/// already applied.
///
/// That cursor is looked up rather than set to N - 1. A later warm restart
/// that has to rebuild the set would target `bk_target(L, N - 1)` =
/// `{N - 2, L2Of(N - 1)}`, and an ordinary block N - 1 has no leaves in the
/// store, so the build would wait for them forever. A real rotation below N
/// is a rotation block, and its leaves are kept.
///
/// Makes at most one query and does not retry it; [`startup_bk`] does.
pub async fn contract_bk_fields<H: BkSetHistory>(
    chain: &EthBridgeContractState,
    stride: u64,
    h: &H,
) -> anyhow::Result<ContractBk> {
    let last_seen = chain.last_seen_block_seq_no;
    let n = chain.last_bk_set_update_seq_no;
    let target = bk_target(last_seen, n, stride);
    // `bk_target` checks leaf [2] of N itself only in the hold window. Its
    // other rows check `L2Of(L)` with L > N, `L3Of(N)`, or `L2Of(1)` with
    // N = 0.
    if target.check != FinalCheck::L2Of(n) {
        return Ok(ContractBk {
            commitment: chain.bk_set_commitment,
            last_bk_set_update_seq_no: n,
            target,
        });
    }
    if chain.prev_bk_set_commitment == [0u8; 32] {
        return Err(Inconsistent(format!(
            "the contract is in the hold window of rotation {n} (last seen block {last_seen}), \
             but its storedPrevBkSetCommitment is zero, which it is only before the first \
             applyBkSetUpdate"
        ))
        .into());
    }
    let below = n - 1;
    let cursor = h
        .last_update_at_or_below(below)
        .await
        .with_context(|| format!("look up the newest BK set update at or below {below}"))?
        .unwrap_or(0);
    if cursor > below {
        return Err(Inconsistent(format!(
            "the newest BK set update at or below {below} is reported at {cursor}"
        ))
        .into());
    }
    Ok(ContractBk {
        commitment: chain.prev_bk_set_commitment,
        last_bk_set_update_seq_no: cursor,
        target,
    })
}

/// An answer from the contract or the server that cannot be right and that
/// no retry changes.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Inconsistent(String);

/// Put the contract's BK fields into a mirror whose windows come from
/// elsewhere: the contract on Resurrect, the local state on a resync.
pub fn apply_contract_bk(state: &mut BridgeState, bk: &ContractBk) {
    state.stored_bk_set_commitment = bk.commitment;
    state.stored_last_bk_set_update_seq_no = bk.last_bk_set_update_seq_no;
}

/// What startup does with `prover_bk_set.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BkFileAction {
    /// The file fits the mirror as it is.
    Keep,
    /// The keys fit the mirror, the rotation cursor does not: only the cursor
    /// changes.
    FixCursor { from: u64, to: u64 },
    /// The file is missing, unreadable or holds another set: the set is built
    /// from GraphQL.
    Rebuild { reason: String },
}

/// Read `prover_bk_set.json`. A missing, unreadable or unparsable file is
/// `None`, so that startup rebuilds the set instead of failing on the file.
pub fn load_bk_file(path: &Path) -> Option<ProverBkSet> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            warn!(path = %path.display(), "no prover BK set file");
            return None;
        },
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot read the prover BK set file");
            return None;
        },
    };
    match serde_json::from_str(&data) {
        Ok(set) => Some(set),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot parse the prover BK set file");
            None
        },
    }
}

/// Whether `file` fits a mirror with `expected_commitment` and rotation cursor
/// `expected_cursor`. `Keep` needs the commitment, the hash of the keys and
/// the cursor to agree; `FixCursor` all but the cursor; anything else is
/// `Rebuild`.
pub fn classify_bk_file(
    file: Option<&ProverBkSet>,
    expected_commitment: [u8; 32],
    expected_cursor: u64,
) -> BkFileAction {
    let Some(file) = file else {
        return BkFileAction::Rebuild {
            reason: "there is no readable prover BK set file".into(),
        };
    };
    if file.commitment != expected_commitment {
        return BkFileAction::Rebuild {
            reason: format!(
                "the file holds the set with commitment {}, the mirror expects {}",
                hex::encode(file.commitment),
                hex::encode(expected_commitment)
            ),
        };
    }
    if let Err(reason) = keys_hash_to_commitment(file) {
        return BkFileAction::Rebuild {
            reason,
        };
    }
    if file.last_applied_update_seq_no == expected_cursor {
        BkFileAction::Keep
    } else {
        BkFileAction::FixCursor {
            from: file.last_applied_update_seq_no,
            to: expected_cursor,
        }
    }
}

/// The Poseidon hash of the keys of `file` against the commitment it
/// records. A key that is not a curve point is reported before hashing: the
/// hash panics on one.
fn keys_hash_to_commitment(file: &ProverBkSet) -> Result<(), String> {
    let keys = file
        .pubkeys()
        .map_err(|e| format!("the keys in the file do not decode: {e:#}"))?;
    if let Some(index) = keys
        .iter()
        .filter(|(_, key)| !is_curve_point(key))
        .map(|(index, _)| *index)
        .min()
    {
        return Err(format!(
            "signer {index} in the file has a key that is not a curve point"
        ));
    }
    let recomputed = commitment(&keys);
    if recomputed != file.commitment {
        return Err(format!(
            "the keys in the file hash to {}, not to the commitment {} it records",
            hex::encode(recomputed),
            hex::encode(file.commitment)
        ));
    }
    Ok(())
}

/// A 48-byte compressed G1 point, read as the commitment reads keys: big-
/// endian first, then little-endian.
fn is_curve_point(key: &[u8]) -> bool {
    let Ok(bytes) = <[u8; 48]>::try_from(key) else {
        return false;
    };
    bool::from(G1Affine::from_compressed_be(&bytes).is_some())
        || bool::from(G1Affine::from_compressed_le(&bytes).is_some())
}

/// The set the prover starts with and how it was obtained.
#[derive(Clone, Debug)]
pub struct ResolvedBk {
    pub prover_bk_set: ProverBkSet,
    pub action: BkFileAction,
}

/// The set the prover starts with, for a mirror with `expected_commitment`
/// and rotation cursor `expected_cursor`. A file that fits is kept; a file
/// whose only fault is the cursor gets the mirror's cursor; otherwise the set
/// is built at `target` and must have `expected_commitment`. The result always
/// carries `expected_cursor` as `last_applied_update_seq_no`. Nothing is
/// written here.
pub async fn resolve_bk_set<H: BkSetHistory>(
    file: Option<ProverBkSet>,
    expected_commitment: [u8; 32],
    expected_cursor: u64,
    target: BkTarget,
    h: &H,
    policy: &BuildPolicy,
) -> Result<ResolvedBk, BkStartupError> {
    let action = classify_bk_file(file.as_ref(), expected_commitment, expected_cursor);
    let prover_bk_set = match (file, &action) {
        (Some(file), BkFileAction::Keep) => {
            info!(
                commitment = %hex::encode(file.commitment),
                cursor = expected_cursor,
                "the prover BK set file fits the mirror"
            );
            file
        },
        (
            Some(mut file),
            &BkFileAction::FixCursor {
                from,
                to,
            },
        ) => {
            warn!(
                from,
                to,
                "the prover BK set file holds the expected keys under another rotation cursor; \
                 fixing the cursor"
            );
            file.last_applied_update_seq_no = to;
            file
        },
        _ => {
            if let BkFileAction::Rebuild {
                reason,
            } = &action
            {
                warn!(
                    %reason,
                    height = target.height,
                    check = ?target.check,
                    "building the prover BK set from GraphQL"
                );
            }
            let built = build_bk_set_with_retry(h, &target, policy).await?;
            let set = ProverBkSet::from_pubkeys(&built.pubkeys, expected_cursor);
            if set.commitment != expected_commitment {
                return Err(BkStartupError::ContractMismatch {
                    height: built.height,
                    expected: expected_commitment,
                    built: set.commitment,
                });
            }
            info!(
                commitment = %hex::encode(set.commitment),
                height = built.height,
                cursor = expected_cursor,
                "built the prover BK set from GraphQL"
            );
            set
        },
    };
    Ok(ResolvedBk {
        prover_bk_set,
        action,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum BkStartupError {
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The set passed every check against the leaves, so the contract
    /// follows another network or another set: not a failure to retry.
    #[error(
        "the BK set built from GraphQL at height {height} has commitment {}, but the contract \
         expects {}: the contract follows another network or another BK set",
        hex::encode(built),
        hex::encode(expected)
    )]
    ContractMismatch {
        height: u64,
        expected: [u8; 32],
        built: [u8; 32],
    },
    /// The contract's BK fields could not be completed from GraphQL.
    #[error("take the BK fields from the contract: {0:#}")]
    ContractFields(anyhow::Error),
    /// Startup routing found local state and contract inconsistent.
    #[error("startup routing STOP: {reason}")]
    Stop { reason: String },
    /// A state file could not be written or moved aside.
    #[error("write the prover state files: {0:#}")]
    Write(anyhow::Error),
}

/// Move a `prover_bk_set.json` that startup does not use to
/// `<file name>.mismatch-<now_unix>` in the same directory, for inspection.
/// `Ok(None)` when there is no file. Never deletes anything: when that name
/// is taken, `.1`, `.2`, ... is appended.
pub fn backup_mismatched_bk_file(path: &Path, now_unix: u64) -> anyhow::Result<Option<PathBuf>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {},
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("inspect {}", path.display())),
    }
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let mut base = name.to_os_string();
    base.push(format!(".mismatch-{now_unix}"));
    let mut backup = path.with_file_name(&base);
    let mut n = 0u32;
    while std::fs::symlink_metadata(&backup).is_ok() {
        n += 1;
        let mut next = OsString::from(&base);
        next.push(format!(".{n}"));
        backup = path.with_file_name(next);
    }
    std::fs::rename(path, &backup)
        .with_context(|| format!("rename {} to {}", path.display(), backup.display()))?;
    warn!(
        path = %path.display(),
        backup = %backup.display(),
        "moved aside the prover BK set file that does not fit"
    );
    Ok(Some(backup))
}

/// What `daemon-live` starts the driver with.
#[derive(Debug)]
pub struct StartupBk {
    /// The mirror the driver starts from. On Cold it is the local state as
    /// loaded, not yet seeded: the driver seeds it and writes it.
    pub state: BridgeState,
    pub seed_policy: SeedPolicy,
    /// The contract's BK fields for the seed: `Some` on Cold only.
    pub seed_bk: Option<SeedBk>,
    /// Carries the commitment and the rotation cursor of `state`, or of
    /// `seed_bk` on Cold.
    pub prover_bk_set: ProverBkSet,
}

/// The mirror a startup decision starts with and the set it needs.
struct MirrorPlan {
    state: BridgeState,
    /// The mirror is built here, not read from disk, and is written after
    /// the set.
    new_mirror: bool,
    seed_policy: SeedPolicy,
    seed_bk: Option<SeedBk>,
    commitment: [u8; 32],
    cursor: u64,
    target: BkTarget,
}

impl MirrorPlan {
    /// A mirror that takes its BK fields from the contract.
    fn with_contract_bk(mut state: BridgeState, bk: &ContractBk) -> Self {
        apply_contract_bk(&mut state, bk);
        Self {
            state,
            new_mirror: true,
            seed_policy: SeedPolicy::Resume,
            seed_bk: None,
            commitment: bk.commitment,
            cursor: bk.last_bk_set_update_seq_no,
            target: bk.target,
        }
    }
}

/// The mirror and the BK set the driver starts with, for the `decision`
/// startup routing took on `local` and `chain`:
///
/// * WarmResume keeps the local mirror: the set must have its commitment and
///   cursor, and is built for `bk_target(L, N)` of that mirror;
/// * Resurrect and a resync of the BK fields put the contract's BK fields
///   ([`contract_bk_fields`]) into their mirror;
/// * Cold keeps the local state, which the driver seeds, and hands it the
///   contract's BK fields as `seed_bk`;
/// * Stop is an error.
///
/// The set comes from [`resolve_bk_set`]. Nothing is written before it is in
/// hand. Then a set file that did not fit is moved aside, and the set is
/// written when it changed or the mirror is new, followed by the mirror if
/// it is new ([`persist_bk_pair`]). Cold writes no mirror: the driver writes
/// it after its first ack.
///
/// The contract lookup is retried like the build: failed queries with the
/// policy's backoff, within its transient budget; a server that does not
/// know a queried field and an answer that cannot be right fail at once.
pub async fn startup_bk<H: BkSetHistory>(
    decision: StartupDecision,
    local: BridgeState,
    chain: &EthBridgeContractState,
    paths: &StatePaths,
    stride: u64,
    h: &H,
    policy: &BuildPolicy,
) -> Result<StartupBk, BkStartupError> {
    // `waited` is what the contract lookup slept.
    let (plan, waited) = match decision {
        StartupDecision::Stop {
            reason,
        } => {
            return Err(BkStartupError::Stop {
                reason,
            })
        },
        StartupDecision::WarmResume => {
            let last_seen = local.stored_last_seen_block_seq_no;
            let last_bk = local.stored_last_bk_set_update_seq_no;
            if last_seen < chain.last_seen_block_seq_no || last_bk < chain.last_bk_set_update_seq_no
            {
                info!(
                    local_last_seen = last_seen,
                    chain_last_seen = chain.last_seen_block_seq_no,
                    local_bk_upd = last_bk,
                    chain_bk_upd = chain.last_bk_set_update_seq_no,
                    "startup: WarmResume — local cursor is behind the contract in the BK-set hold \
                     window; bundles the chain already verified are acked before the rotation"
                );
            } else {
                info!(
                    last_seen,
                    "startup: WarmResume — local state matches the chain"
                );
            }
            let plan = MirrorPlan {
                commitment: local.stored_bk_set_commitment,
                cursor: last_bk,
                target: bk_target(last_seen, last_bk, stride),
                state: local,
                new_mirror: false,
                seed_policy: SeedPolicy::Resume,
                seed_bk: None,
            };
            (plan, Duration::ZERO)
        },
        StartupDecision::Resurrect {
            fresh_state,
        } => {
            info!(
                chain_last_seen = chain.last_seen_block_seq_no,
                chain_bk_upd = chain.last_bk_set_update_seq_no,
                "startup: Resurrect — rebuilding the mirror from the contract"
            );
            let (bk, waited) = contract_bk_with_retry(chain, stride, h, policy).await?;
            (MirrorPlan::with_contract_bk(*fresh_state, &bk), waited)
        },
        StartupDecision::ResyncBkFromContract {
            state,
        } => {
            info!(
                last_seen = state.stored_last_seen_block_seq_no,
                local_bk_upd = state.stored_last_bk_set_update_seq_no,
                chain_bk_upd = chain.last_bk_set_update_seq_no,
                "startup: the contract applied BK set updates the local mirror has not; taking \
                 its BK fields over the local windows"
            );
            let (bk, waited) = contract_bk_with_retry(chain, stride, h, policy).await?;
            (MirrorPlan::with_contract_bk(*state, &bk), waited)
        },
        StartupDecision::Cold {
            policy: seed_policy,
        } => {
            let (bk, waited) = contract_bk_with_retry(chain, stride, h, policy).await?;
            info!(
                ?seed_policy,
                commitment = %hex::encode(bk.commitment),
                last_bk_set_update_seq_no = bk.last_bk_set_update_seq_no,
                "startup: Cold — seeding with the contract's BK fields"
            );
            let plan = MirrorPlan {
                state: local,
                new_mirror: false,
                seed_policy,
                seed_bk: Some(SeedBk {
                    commitment: bk.commitment,
                    last_bk_set_update_seq_no: bk.last_bk_set_update_seq_no,
                }),
                commitment: bk.commitment,
                cursor: bk.last_bk_set_update_seq_no,
                target: bk.target,
            };
            (plan, waited)
        },
    };

    // The contract lookup spent part of a bounded transient budget.
    let policy = BuildPolicy {
        transient_budget: policy.transient_budget.map(|b| b.saturating_sub(waited)),
        ..*policy
    };
    let resolved = resolve_bk_set(
        load_bk_file(&paths.prover_bk_set_json),
        plan.commitment,
        plan.cursor,
        plan.target,
        h,
        &policy,
    )
    .await?;

    // Nothing below waits: a shutdown that drops this future while it waits
    // above leaves the files as they were.
    if matches!(resolved.action, BkFileAction::Rebuild { .. }) {
        backup_mismatched_bk_file(&paths.prover_bk_set_json, now_unix())
            .map_err(BkStartupError::Write)?;
    }
    if resolved.action != BkFileAction::Keep || plan.new_mirror {
        persist_bk_pair(
            paths,
            &resolved.prover_bk_set,
            plan.new_mirror.then_some(&plan.state),
        )
        .map_err(|e| BkStartupError::Write(e.into()))?;
    }
    Ok(StartupBk {
        state: plan.state,
        seed_policy: plan.seed_policy,
        seed_bk: plan.seed_bk,
        prover_bk_set: resolved.prover_bk_set,
    })
}

/// [`contract_bk_fields`], with failed queries retried on the policy's
/// backoff, and the time slept. A server that does not know a queried field
/// and an [`Inconsistent`] answer fail at once.
async fn contract_bk_with_retry<H: BkSetHistory>(
    chain: &EthBridgeContractState,
    stride: u64,
    h: &H,
    policy: &BuildPolicy,
) -> Result<(ContractBk, Duration), BkStartupError> {
    let mut waited = Duration::ZERO;
    let mut backoff = policy.backoff_initial;
    loop {
        let e = match contract_bk_fields(chain, stride, h).await {
            Ok(bk) => return Ok((bk, waited)),
            Err(e) => e,
        };
        if e.downcast_ref::<UnknownFieldError>().is_some()
            || e.downcast_ref::<Inconsistent>().is_some()
        {
            return Err(BkStartupError::ContractFields(e));
        }
        let sleep = match policy.transient_budget {
            None => backoff,
            Some(budget) if waited >= budget => {
                return Err(BkStartupError::ContractFields(
                    e.context(format!("kept failing for {waited:?}")),
                ))
            },
            Some(budget) => backoff.min(budget - waited),
        };
        let reason = format!("{e:#}");
        warn!(
            error = %reason,
            retry_in = ?sleep,
            "looking up the contract's BK fields failed, will retry"
        );
        tokio::time::sleep(sleep).await;
        waited = waited.saturating_add(sleep);
        backoff = backoff
            .saturating_mul(policy.backoff_multiplier)
            .min(policy.backoff_max);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Run `startup` until it finishes or `shutdown` resolves. `Ok(None)` is the
/// shutdown: [`startup_bk`] is then dropped while it waits, before it has
/// written anything.
pub async fn startup_bk_or_shutdown<F, S>(
    startup: F,
    shutdown: S,
) -> Result<Option<StartupBk>, BkStartupError>
where
    F: Future<Output = Result<StartupBk, BkStartupError>>,
    S: Future<Output = ()>,
{
    tokio::select! {
        started = startup => started.map(Some),
        () = shutdown => {
            info!("shutdown while building the prover BK set; no state file was written");
            Ok(None)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use alloy::primitives::U256;
    use bridge_gql_fetcher::gql_client::{UnknownFieldError, BLOCK_ID_TREE_LEAF_COUNT};
    use bridge_prover_lib::{
        bk_set_replay::{commitment, BkSetSnapshot, BkSetUpdateRow, BlockLeaves, FinalCheck},
        bridge_state::{HistoryWindow, MAX_LAYERS},
        live_driver::{BkUpdateProofArtifacts, BundleFinalizationType},
        transcript::TranscriptKind,
    };
    use halo2_base::halo2_proofs::halo2curves::{
        bls12_381::{Fr as BlsScalar, G1Affine, G1},
        group::Curve,
    };
    use tokio::time::Instant;

    use super::*;
    use crate::{
        startup_decide::{decide, DecideInputs},
        types::BkSetUpdateData,
    };

    const STRIDE: u64 = 16384;
    const W: usize = 4;

    const ADD: u32 = 0;
    const REMOVE: u32 = 1;
    /// Any change kind that leaves the set alone (the `Future*` ones).
    const OTHER: u32 = 2;

    // S0 --40000--> S1 --50000--> S2. The rows at 15000, 20000 and 60000
    // carry `Future*` changes only.
    const S0: &[(u16, u64)] = &[(0, 1), (1, 2)];
    const S1: &[(u16, u64)] = &[(0, 1), (1, 2), (2, 3)];
    const S2: &[(u16, u64)] = &[(0, 1), (2, 3)];

    fn point(seed: u64) -> G1Affine {
        (G1::generator() * BlsScalar::from(seed)).to_affine()
    }

    fn set(entries: &[(u16, u64)]) -> HashMap<u16, Vec<u8>> {
        entries
            .iter()
            .map(|(i, s)| (*i, point(*s).to_compressed_be().to_vec()))
            .collect()
    }

    fn c(entries: &[(u16, u64)]) -> [u8; 32] {
        commitment(&set(entries))
    }

    fn file(entries: &[(u16, u64)], cursor: u64) -> ProverBkSet {
        ProverBkSet::from_pubkeys(&set(entries), cursor)
    }

    /// One change in the node's wire format, with a 96-byte uncompressed key.
    fn row(height: u64, variant: u32, index: u16, seed: u64) -> BkSetUpdateRow {
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u64.to_le_bytes());
        blob.extend_from_slice(&variant.to_le_bytes());
        blob.extend_from_slice(&index.to_le_bytes());
        blob.extend_from_slice(&96u64.to_le_bytes());
        blob.extend_from_slice(&point(seed).to_uncompressed_be());
        BkSetUpdateRow {
            height: Some(height),
            bk_set_update_hex: hex::encode(blob),
        }
    }

    fn leaves(l2: &[(u16, u64)], l3: &[(u16, u64)]) -> BlockLeaves {
        let mut l = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        l[2] = c(l2);
        l[3] = c(l3);
        l
    }

    /// Thread 0 of a gql-server, counting what it is asked.
    #[derive(Default)]
    struct FakeHistory {
        snapshot: Option<BkSetSnapshot>,
        rows: Vec<BkSetUpdateRow>,
        leaves: BTreeMap<u64, BlockLeaves>,
        /// `last_update_at_or_below` fails this many times before it answers.
        cursor_failures: AtomicUsize,
        /// `last_update_at_or_below` answers as a server without the field.
        cursor_unknown_field: bool,
        snapshot_calls: AtomicUsize,
        cursor_calls: AtomicUsize,
        other_calls: AtomicUsize,
    }

    impl FakeHistory {
        /// Every build starts with a snapshot.
        fn snapshot_calls(&self) -> usize {
            self.snapshot_calls.load(Ordering::SeqCst)
        }

        fn cursor_calls(&self) -> usize {
            self.cursor_calls.load(Ordering::SeqCst)
        }

        fn calls(&self) -> usize {
            self.snapshot_calls() + self.cursor_calls() + self.other_calls.load(Ordering::SeqCst)
        }
    }

    impl BkSetHistory for FakeHistory {
        async fn latest_bk_set(&self) -> anyhow::Result<Option<BkSetSnapshot>> {
            self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.clone())
        }

        async fn bk_set_updates(
            &self,
            height_start: u64,
            height_end: u64,
        ) -> anyhow::Result<Vec<BkSetUpdateRow>> {
            self.other_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .rows
                .iter()
                .filter(|r| {
                    r.height
                        .is_some_and(|h| (height_start..=height_end).contains(&h))
                })
                .cloned()
                .collect())
        }

        async fn block_leaves(&self, height: u64) -> anyhow::Result<Option<BlockLeaves>> {
            self.other_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.leaves.get(&height).copied())
        }

        async fn last_update_at_or_below(&self, height: u64) -> anyhow::Result<Option<u64>> {
            self.cursor_calls.fetch_add(1, Ordering::SeqCst);
            if self.cursor_unknown_field {
                return Err(anyhow::Error::new(UnknownFieldError {
                    field: "bkSetUpdates".into(),
                }));
            }
            let failures_left = self.cursor_failures.load(Ordering::SeqCst);
            if failures_left > 0 {
                self.cursor_failures
                    .store(failures_left - 1, Ordering::SeqCst);
                anyhow::bail!("connection reset by peer");
            }
            Ok(self
                .rows
                .iter()
                .filter_map(|r| r.height)
                .filter(|&h| h <= height)
                .max())
        }
    }

    /// A gql-server the scenario must not ask at all.
    struct NoGraphql;

    impl BkSetHistory for NoGraphql {
        async fn latest_bk_set(&self) -> anyhow::Result<Option<BkSetSnapshot>> {
            panic!("asked for latestBkSet")
        }

        async fn bk_set_updates(&self, _: u64, _: u64) -> anyhow::Result<Vec<BkSetUpdateRow>> {
            panic!("asked for bkSetUpdates")
        }

        async fn block_leaves(&self, _: u64) -> anyhow::Result<Option<BlockLeaves>> {
            panic!("asked for block leaves")
        }

        async fn last_update_at_or_below(&self, _: u64) -> anyhow::Result<Option<u64>> {
            panic!("asked for the last rotation")
        }
    }

    /// The chain the scenarios run on: rotations at 15000, 20000, 40000,
    /// 50000 and 60000, bundle boundaries every [`STRIDE`], a snapshot at
    /// 70000. Only rotation blocks and boundaries have leaves.
    fn world() -> FakeHistory {
        let mut h = FakeHistory {
            snapshot: Some(BkSetSnapshot {
                anchor_height: 70000,
                signers: set(S2),
            }),
            rows: vec![
                row(15000, OTHER, 0, 1),
                row(20000, OTHER, 0, 1),
                row(40000, ADD, 2, 3),
                row(50000, REMOVE, 1, 2),
                row(60000, OTHER, 0, 1),
            ],
            ..Default::default()
        };
        for (height, l2, l3) in [
            (15000, S0, S0),
            (16384, S0, S0),
            (20000, S0, S0),
            (32768, S0, S0),
            (40000, S0, S1),
            (49152, S1, S1),
            (50000, S1, S2),
            (60000, S2, S2),
            (65536, S2, S2),
        ] {
            h.leaves.insert(height, leaves(l2, l3));
        }
        h
    }

    /// Fails at once on anything a retry would wait for, so a scenario that
    /// would hang reports instead.
    fn no_wait() -> BuildPolicy {
        BuildPolicy {
            data_attempts: 1,
            transient_budget: Some(Duration::ZERO),
            ..BuildPolicy::default()
        }
    }

    /// Layer 1 holds one hash per bundle height; the other layers are empty.
    fn windows(bundles: &[u64]) -> Vec<HistoryWindow> {
        (0..MAX_LAYERS)
            .map(|layer| {
                let mut w = HistoryWindow::new(W);
                if layer == 0 {
                    for &height in bundles {
                        let mut hash = [0u8; 32];
                        hash[..8].copy_from_slice(&height.to_le_bytes());
                        w.append(hash, height);
                    }
                }
                w
            })
            .collect()
    }

    fn chain_state(
        last_seen: u64,
        last_bk: u64,
        current: &[(u16, u64)],
        prev: [u8; 32],
        bundles: &[u64],
    ) -> EthBridgeContractState {
        let ws = windows(bundles);
        EthBridgeContractState {
            last_seen_block_seq_no: last_seen,
            bk_set_commitment: c(current),
            last_bk_set_update_seq_no: last_bk,
            prev_bk_set_commitment: prev,
            genesis_prev_max_level_layer_hash: [0u8; 32],
            layer_windows: std::array::from_fn(|i| ws[i].clone()),
        }
    }

    fn mirror(
        last_seen: u64,
        last_bk: u64,
        current: &[(u16, u64)],
        bundles: &[u64],
    ) -> BridgeState {
        let mut s = BridgeState::new(W);
        s.layer_windows = windows(bundles);
        s.stored_bk_set_commitment = c(current);
        s.stored_last_seen_block_seq_no = last_seen;
        s.stored_last_bk_set_update_seq_no = last_bk;
        s.initialized = true;
        s.anchor_level = 1;
        s
    }

    #[tokio::test]
    async fn hold_window_uses_prev_and_the_rotation_before_n() {
        let h = world();
        // Rotation 40000 is on the contract; the bundle at 32768 is still owed.
        let chain = chain_state(16384, 40000, S1, c(S0), &[16384]);
        let bk = contract_bk_fields(&chain, STRIDE, &h).await.unwrap();
        assert_eq!(bk, ContractBk {
            commitment: c(S0),
            last_bk_set_update_seq_no: 20000,
            target: BkTarget {
                height: 39999,
                check: FinalCheck::L2Of(40000),
            },
        });
        let built = resolve_bk_set(
            None,
            bk.commitment,
            bk.last_bk_set_update_seq_no,
            bk.target,
            &h,
            &no_wait(),
        )
        .await
        .unwrap();
        assert_eq!(built.prover_bk_set.pubkeys().unwrap(), set(S0));
        assert_eq!(built.prover_bk_set.last_applied_update_seq_no, 20000);

        // A warm restart of that mirror that has to rebuild its set targets
        // the rotation block at the cursor, whose leaves the store keeps.
        let warm = bk_target(16384, bk.last_bk_set_update_seq_no, STRIDE);
        assert_eq!(warm, BkTarget {
            height: 20000,
            check: FinalCheck::L3Of(20000),
        });
        resolve_bk_set(None, c(S0), 20000, warm, &h, &no_wait())
            .await
            .unwrap();
        // With N - 1 as the cursor it would target an ordinary block, which
        // has no leaves: a wait that never ends.
        let naive = bk_target(16384, 39999, STRIDE);
        assert_eq!(naive, BkTarget {
            height: 39998,
            check: FinalCheck::L2Of(39999),
        });
        let e = resolve_bk_set(None, c(S0), 39999, naive, &h, &no_wait())
            .await
            .unwrap_err();
        assert!(
            matches!(e, BkStartupError::Build(BuildError::Transient { .. })),
            "{e}"
        );

        // No rotation below N at all: cursor 0.
        let only_n = FakeHistory {
            rows: vec![row(40000, ADD, 2, 3)],
            ..Default::default()
        };
        let bk = contract_bk_fields(&chain, STRIDE, &only_n).await.unwrap();
        assert_eq!(bk.commitment, c(S0));
        assert_eq!(bk.last_bk_set_update_seq_no, 0);
    }

    #[tokio::test]
    async fn outside_the_hold_window_takes_the_current_set_and_n() {
        let h = FakeHistory::default();
        let chain = chain_state(49152, 40000, S1, c(S0), &[16384, 32768, 49152]);
        assert_eq!(
            contract_bk_fields(&chain, STRIDE, &h).await.unwrap(),
            ContractBk {
                commitment: c(S1),
                last_bk_set_update_seq_no: 40000,
                target: BkTarget {
                    height: 49151,
                    check: FinalCheck::L2Of(49152),
                },
            }
        );
        // Cold after a rotation applied before the first bundle.
        let chain = chain_state(16384, 20000, S0, c(S0), &[]);
        assert_eq!(
            contract_bk_fields(&chain, STRIDE, &h).await.unwrap(),
            ContractBk {
                commitment: c(S0),
                last_bk_set_update_seq_no: 20000,
                target: BkTarget {
                    height: 20000,
                    check: FinalCheck::L3Of(20000),
                },
            }
        );
        assert_eq!(h.calls(), 0);
    }

    #[tokio::test]
    async fn hold_window_without_a_previous_commitment_is_an_error() {
        let h = world();
        let chain = chain_state(16384, 40000, S1, [0u8; 32], &[16384]);
        let e = contract_bk_fields(&chain, STRIDE, &h).await.unwrap_err();
        assert!(e.to_string().contains("storedPrevBkSetCommitment"), "{e}");
        assert_eq!(h.calls(), 0);
    }

    #[test]
    fn same_commitment_other_cursor_fixes_cursor() {
        // A rotation with `Future*` changes only moved the file's cursor to
        // 20000; the mirror stayed at 15000.
        let f = file(S0, 20000);
        let c0 = c(S0);
        assert_eq!(
            classify_bk_file(Some(&f), c0, 15000),
            BkFileAction::FixCursor {
                from: 20000,
                to: 15000
            }
        );
        assert_eq!(classify_bk_file(Some(&f), c0, 20000), BkFileAction::Keep);
    }

    #[tokio::test]
    async fn legacy_file_with_cursor_zero_is_fixed_not_rebuilt() {
        // Files written before the cursor was kept say 0 after a cold start.
        let f = file(S0, 0);
        assert_eq!(
            classify_bk_file(Some(&f), c(S0), 15000),
            BkFileAction::FixCursor {
                from: 0,
                to: 15000
            }
        );
        let h = FakeHistory::default();
        let r = resolve_bk_set(
            Some(f.clone()),
            c(S0),
            15000,
            bk_target(16384, 15000, STRIDE),
            &h,
            &no_wait(),
        )
        .await
        .unwrap();
        assert_eq!(r.action, BkFileAction::FixCursor {
            from: 0,
            to: 15000
        });
        assert_eq!(r.prover_bk_set.last_applied_update_seq_no, 15000);
        assert_eq!(r.prover_bk_set.commitment, f.commitment);
        assert_eq!(r.prover_bk_set.pubkeys_hex, f.pubkeys_hex);
        assert_eq!(h.calls(), 0);
    }

    #[test]
    fn other_commitment_rebuilds() {
        let f = file(S1, 15000);
        assert!(matches!(
            classify_bk_file(Some(&f), c(S0), 15000),
            BkFileAction::Rebuild { .. }
        ));
        assert!(matches!(
            classify_bk_file(None, c(S0), 15000),
            BkFileAction::Rebuild { .. }
        ));
    }

    #[test]
    fn self_inconsistent_file_rebuilds() {
        // The expected commitment over other keys.
        let mut f = file(S1, 15000);
        f.commitment = c(S0);
        match classify_bk_file(Some(&f), c(S0), 15000) {
            BkFileAction::Rebuild {
                reason,
            } => assert!(reason.contains("hash"), "{reason}"),
            other => panic!("{other:?}"),
        }
        // A key that is not a curve point is a rebuild, not a panic in the
        // hash.
        let mut bad = file(S0, 15000);
        bad.pubkeys_hex.insert(1, hex::encode([0xffu8; 48]));
        assert!(matches!(
            classify_bk_file(Some(&bad), c(S0), 15000),
            BkFileAction::Rebuild { .. }
        ));
        bad.pubkeys_hex.insert(1, "abcd".into());
        assert!(matches!(
            classify_bk_file(Some(&bad), c(S0), 15000),
            BkFileAction::Rebuild { .. }
        ));
    }

    #[test]
    fn truncated_file_loads_as_none_and_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prover_bk_set.json");
        std::fs::write(&path, "{").unwrap();
        assert!(load_bk_file(&path).is_none());

        let first = backup_mismatched_bk_file(&path, 123).unwrap();
        assert_eq!(
            first,
            Some(dir.path().join("prover_bk_set.json.mismatch-123"))
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(first.as_ref().unwrap()).unwrap(),
            "{"
        );

        // Nothing left to back up, and a missing file loads as `None` too.
        assert_eq!(backup_mismatched_bk_file(&path, 124).unwrap(), None);
        assert!(load_bk_file(&path).is_none());

        // A second backup in the same second keeps the first.
        std::fs::write(&path, "[").unwrap();
        let second = backup_mismatched_bk_file(&path, 123).unwrap().unwrap();
        assert_ne!(Some(&second), first.as_ref());
        assert_eq!(std::fs::read_to_string(first.unwrap()).unwrap(), "{");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "[");
    }

    #[tokio::test]
    async fn rebuild_must_match_the_contract() {
        let h = world();
        // The set that signs 49152 is S1; the contract is taken to expect S0.
        let target = bk_target(49152, 40000, STRIDE);
        let e = resolve_bk_set(None, c(S0), 40000, target, &h, &no_wait())
            .await
            .unwrap_err();
        match e {
            BkStartupError::ContractMismatch {
                height,
                expected,
                built,
            } => {
                assert_eq!(height, 49151);
                assert_eq!(expected, c(S0));
                assert_eq!(built, c(S1));
            },
            other => panic!("{other}"),
        }
    }

    /// The contract stores a rotation's commitment as the number the relayer
    /// submits, and `read_full_state` hands it back with `to_le_bytes`. A set
    /// built from the leaves must compare equal to what comes back.
    #[tokio::test]
    async fn contract_commitment_bytes_match_a_built_set() {
        let rotation = BkUpdateProofArtifacts {
            block_seq_no: 50000,
            block_height: 50000,
            last_seen_bk_update_seq_no: 49152,
            block_id_be: [0; 32],
            fin_type: BundleFinalizationType::Primary,
            old_bk_set_commitment_be: c(S1),
            new_bk_set_commitment_be: c(S2),
            merkle_sibling_h01_be: [0; 32],
            merkle_sibling_h4_7_be: [0; 32],
            merkle_sibling_h8_15_be: [0; 32],
            transcript_kind: TranscriptKind::Blake2b,
            attestation_proof: Vec::new(),
            new_pubkeys: HashMap::new(),
            attestation_proof_gen_ms: 0,
        };
        let stored: U256 = BkSetUpdateData::from(&rotation).new_commitment_l3;
        // `EthBridgeClient::read_full_state`.
        let read_back = stored.to_le_bytes::<32>();

        let h = world();
        let r = resolve_bk_set(
            None,
            read_back,
            50000,
            bk_target(65536, 50000, STRIDE),
            &h,
            &no_wait(),
        )
        .await
        .unwrap();
        assert_eq!(r.prover_bk_set.commitment, read_back);
        assert_eq!(
            classify_bk_file(Some(&r.prover_bk_set), read_back, 50000),
            BkFileAction::Keep
        );
    }

    fn json(state: &BridgeState) -> serde_json::Value {
        serde_json::to_value(state).unwrap()
    }

    fn decision_kind(d: &StartupDecision) -> &'static str {
        match d {
            StartupDecision::Cold {
                ..
            } => "cold",
            StartupDecision::WarmResume => "warm",
            StartupDecision::Resurrect {
                ..
            } => "resurrect",
            StartupDecision::ResyncBkFromContract {
                ..
            } => "resync",
            StartupDecision::Stop {
                ..
            } => "stop",
        }
    }

    fn decide_on(local: &BridgeState, chain: &EthBridgeContractState) -> StartupDecision {
        decide(DecideInputs {
            local,
            chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 1,
        })
    }

    fn load_mirror(paths: &StatePaths) -> BridgeState {
        BridgeState::load(paths.prover_state_json.to_str().unwrap(), W).unwrap()
    }

    /// Files in `dir` other than the pair of state files.
    fn other_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "prover_state.json" && n != "prover_bk_set.json")
            .collect();
        names.sort();
        names
    }

    fn backups(dir: &Path) -> usize {
        other_files(dir)
            .iter()
            .filter(|n| n.starts_with("prover_bk_set.json.mismatch-"))
            .count()
    }

    /// One restart on the files in `paths`: the decision `startup_decide`
    /// takes on them and what [`startup_bk`] starts the driver with.
    async fn restart(
        name: &str,
        paths: &StatePaths,
        chain: &EthBridgeContractState,
        h: &FakeHistory,
    ) -> (&'static str, StartupBk) {
        let local = load_mirror(paths);
        let decision = decide_on(&local, chain);
        let kind = decision_kind(&decision);
        let started = startup_bk(decision, local, chain, paths, STRIDE, h, &no_wait())
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        // The set fits the BK fields the driver starts with: the mirror's,
        // or the seed's on Cold.
        let (commitment, cursor) = match started.seed_bk {
            Some(bk) => (bk.commitment, bk.last_bk_set_update_seq_no),
            None => (
                started.state.stored_bk_set_commitment,
                started.state.stored_last_bk_set_update_seq_no,
            ),
        };
        assert_eq!(
            classify_bk_file(Some(&started.prover_bk_set), commitment, cursor),
            BkFileAction::Keep,
            "{name}"
        );
        // Both are what is on disk; Cold leaves the mirror to the driver.
        let on_disk = load_bk_file(&paths.prover_bk_set_json)
            .unwrap_or_else(|| panic!("{name}: no prover BK set on disk"));
        assert_eq!(
            (
                on_disk.commitment,
                &on_disk.pubkeys_hex,
                on_disk.last_applied_update_seq_no
            ),
            (
                started.prover_bk_set.commitment,
                &started.prover_bk_set.pubkeys_hex,
                started.prover_bk_set.last_applied_update_seq_no
            ),
            "{name}"
        );
        if paths.prover_state_json.exists() {
            assert_eq!(json(&load_mirror(paths)), json(&started.state), "{name}");
        } else {
            assert_eq!(kind, "cold", "{name}: no mirror on disk");
        }
        (kind, started)
    }

    /// What a restart did with the set file, read from its effects: a build
    /// asks GraphQL for a snapshot, a cursor fix keeps the keys under another
    /// cursor, a keep leaves the set as it was.
    fn observed_action(
        before: Option<&ProverBkSet>,
        after: &ProverBkSet,
        built: bool,
    ) -> &'static str {
        if built {
            return "rebuild";
        }
        let before = before.expect("a set that was not built comes from the file");
        assert_eq!(
            (before.commitment, &before.pubkeys_hex),
            (after.commitment, &after.pubkeys_hex)
        );
        if before.last_applied_update_seq_no == after.last_applied_update_seq_no {
            "keep"
        } else {
            "fix-cursor"
        }
    }

    /// Put the mirror file back as it was: what a restart cut short between
    /// the set and the mirror leaves.
    fn restore(path: &Path, bytes: Option<&[u8]>) {
        match bytes {
            Some(bytes) => std::fs::write(path, bytes).unwrap(),
            None => {
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                }
            },
        }
    }

    struct Case {
        name: &'static str,
        chain: EthBridgeContractState,
        mirror_on_disk: Option<BridgeState>,
        set_on_disk: Option<ProverBkSet>,
        decision: &'static str,
        action: &'static str,
    }

    #[tokio::test]
    async fn every_interrupted_write_recovers() {
        let h = world();
        let w1 = [16384];
        let w2 = [16384, 32768];
        let w3 = [16384, 32768, 49152];
        let w4 = [16384, 32768, 49152, 65536];
        let cases = [
            Case {
                name: "resurrect, cut short before any write",
                chain: chain_state(65536, 50000, S2, c(S1), &w4),
                mirror_on_disk: Some(mirror(16384, 15000, S0, &w1)),
                set_on_disk: Some(file(S0, 15000)),
                decision: "resurrect",
                action: "rebuild",
            },
            Case {
                name: "resurrect in the hold window, cut short before any write",
                chain: chain_state(16384, 40000, S1, c(S0), &w1),
                mirror_on_disk: None,
                set_on_disk: None,
                decision: "resurrect",
                action: "rebuild",
            },
            Case {
                name: "persist_driver cut short between the files, rotation changed the set",
                chain: chain_state(32768, 40000, S1, c(S0), &w2),
                mirror_on_disk: Some(mirror(32768, 20000, S0, &w2)),
                set_on_disk: Some(file(S1, 40000)),
                decision: "warm",
                action: "rebuild",
            },
            Case {
                name: "persist_driver cut short between the files, rotation acked at once",
                chain: chain_state(49152, 50000, S2, c(S1), &w3),
                mirror_on_disk: Some(mirror(49152, 40000, S1, &w3)),
                set_on_disk: Some(file(S2, 50000)),
                decision: "warm",
                action: "rebuild",
            },
            Case {
                name: "persist_driver cut short between the files, rotation with Future* only",
                chain: chain_state(16384, 20000, S0, c(S0), &w1),
                mirror_on_disk: Some(mirror(16384, 15000, S0, &w1)),
                set_on_disk: Some(file(S0, 20000)),
                decision: "warm",
                action: "fix-cursor",
            },
            Case {
                name: "persist_driver cut short between the files, bundle ack",
                chain: chain_state(32768, 20000, S0, c(S0), &w2),
                mirror_on_disk: Some(mirror(16384, 20000, S0, &w1)),
                set_on_disk: Some(file(S0, 20000)),
                decision: "resurrect",
                action: "keep",
            },
            Case {
                name: "resync of the BK fields, cut short before any write",
                chain: chain_state(49152, 60000, S2, c(S2), &w3),
                mirror_on_disk: Some(mirror(49152, 40000, S1, &w3)),
                set_on_disk: Some(file(S1, 40000)),
                decision: "resync",
                action: "rebuild",
            },
            Case {
                name: "cold, cut short before the set is written",
                chain: chain_state(16384, 15000, S0, [0u8; 32], &[]),
                mirror_on_disk: None,
                set_on_disk: None,
                decision: "cold",
                action: "rebuild",
            },
        ];

        for case in cases {
            let name = case.name;
            let dir = tempfile::tempdir().unwrap();
            let paths = StatePaths::under(dir.path());
            if let Some(m) = &case.mirror_on_disk {
                m.save(paths.prover_state_json.to_str().unwrap()).unwrap();
            }
            if let Some(s) = &case.set_on_disk {
                s.save(paths.prover_bk_set_json.to_str().unwrap()).unwrap();
            }
            let mirror_before = std::fs::read(&paths.prover_state_json).ok();

            // The restart after the cut.
            let set_before = load_bk_file(&paths.prover_bk_set_json);
            let builds = h.snapshot_calls();
            let (decision, first) = restart(name, &paths, &case.chain, &h).await;
            assert_eq!(decision, case.decision, "{name}");
            let built = h.snapshot_calls() > builds;
            assert_eq!(
                observed_action(set_before.as_ref(), &first.prover_bk_set, built),
                case.action,
                "{name}"
            );
            // A set file that did not fit is kept aside, never deleted.
            let backed_up = usize::from(case.action == "rebuild" && case.set_on_disk.is_some());
            assert_eq!(backups(dir.path()), backed_up, "{name}");

            // That restart is cut short after the set, with the mirror still
            // the one it found: the next one takes the set as it is and
            // builds nothing.
            restore(&paths.prover_state_json, mirror_before.as_deref());
            let set_before = load_bk_file(&paths.prover_bk_set_json);
            let builds = h.snapshot_calls();
            let (decision, second) = restart(name, &paths, &case.chain, &h).await;
            assert_eq!(decision, case.decision, "{name}");
            let built = h.snapshot_calls() > builds;
            assert_eq!(
                observed_action(set_before.as_ref(), &second.prover_bk_set, built),
                "keep",
                "{name}"
            );

            // That restart completes, set then mirror: the next one resumes
            // on both.
            let set_before = load_bk_file(&paths.prover_bk_set_json);
            let (decision, third) = restart(name, &paths, &case.chain, &h).await;
            let resumes = if case.decision == "cold" {
                "cold"
            } else {
                "warm"
            };
            assert_eq!(decision, resumes, "{name}");
            let built = h.snapshot_calls() > builds;
            assert_eq!(
                observed_action(set_before.as_ref(), &third.prover_bk_set, built),
                "keep",
                "{name}"
            );
            assert_eq!(h.snapshot_calls(), builds, "{name}: rebuilt a set on disk");
            assert_eq!(backups(dir.path()), backed_up, "{name}");
        }
    }

    #[tokio::test]
    async fn cold_after_rotations_starts_on_the_contract_set() {
        // Seeded at S = L = 16384. Rotation 20000 added signer 2 before the
        // first bundle: the contract holds S1 under N = 20000, while the seed
        // block is still signed by S0.
        let mut h = FakeHistory {
            snapshot: Some(BkSetSnapshot {
                anchor_height: 30000,
                signers: set(S1),
            }),
            rows: vec![row(20000, ADD, 2, 3)],
            ..Default::default()
        };
        h.leaves.insert(16384, leaves(S0, S0));
        h.leaves.insert(20000, leaves(S0, S1));
        let chain = chain_state(16384, 20000, S1, c(S0), &[]);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        assert!(
            matches!(decision, StartupDecision::Cold {
                policy: SeedPolicy::Explicit(16384)
            }),
            "{decision:?}"
        );

        let started = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &no_wait())
            .await
            .unwrap();
        assert_eq!(
            started.seed_bk,
            Some(SeedBk {
                commitment: c(S1),
                last_bk_set_update_seq_no: 20000,
            })
        );
        assert_eq!(started.seed_policy, SeedPolicy::Explicit(16384));
        assert!(!started.state.initialized);
        assert_eq!(started.prover_bk_set.pubkeys().unwrap(), set(S1));
        assert_eq!(started.prover_bk_set.last_applied_update_seq_no, 20000);
        // The set is written. The mirror is the driver's to write once it
        // has seeded.
        let on_disk = load_bk_file(&paths.prover_bk_set_json).unwrap();
        assert_eq!(
            (on_disk.commitment, on_disk.last_applied_update_seq_no),
            (c(S1), 20000)
        );
        assert!(!paths.prover_state_json.exists());
        assert!(other_files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn resurrect_in_hold_window_takes_the_outgoing_set() {
        let h = world();
        // Rotation 40000 is on the contract; the bundle at 32768, signed by
        // S0, is still owed.
        let chain = chain_state(16384, 40000, S1, c(S0), &[16384]);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        assert_eq!(decision_kind(&decision), "resurrect");

        let started = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &no_wait())
            .await
            .unwrap();
        assert_eq!(
            started.state.stored_bk_set_commitment,
            chain.prev_bk_set_commitment
        );
        // The rotation below N, so that the driver takes N next.
        assert_eq!(started.state.stored_last_bk_set_update_seq_no, 20000);
        assert_eq!(started.state.stored_last_seen_block_seq_no, 16384);
        assert_eq!(started.seed_policy, SeedPolicy::Resume);
        assert_eq!(started.seed_bk, None);
        assert_eq!(started.prover_bk_set.pubkeys().unwrap(), set(S0));
        assert_eq!(started.prover_bk_set.last_applied_update_seq_no, 20000);

        // Both files are written, and a restart resumes on them.
        let saved = load_mirror(&paths);
        assert_eq!(json(&saved), json(&started.state));
        assert!(matches!(
            decide_on(&saved, &chain),
            StartupDecision::WarmResume
        ));
    }

    #[tokio::test]
    async fn warm_resume_with_good_file_never_queries_graphql() {
        let w3 = [16384, 32768, 49152];
        let chain = chain_state(49152, 40000, S1, c(S0), &w3);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        mirror(49152, 40000, S1, &w3)
            .save(paths.prover_state_json.to_str().unwrap())
            .unwrap();
        file(S1, 40000)
            .save(paths.prover_bk_set_json.to_str().unwrap())
            .unwrap();
        let set_bytes = std::fs::read(&paths.prover_bk_set_json).unwrap();
        let mirror_bytes = std::fs::read(&paths.prover_state_json).unwrap();
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        assert_eq!(decision_kind(&decision), "warm");

        let started = startup_bk(
            decision,
            local,
            &chain,
            &paths,
            STRIDE,
            &NoGraphql,
            &no_wait(),
        )
        .await
        .unwrap();
        assert_eq!(started.seed_policy, SeedPolicy::Resume);
        assert_eq!(started.seed_bk, None);
        assert_eq!(
            (
                started.state.stored_bk_set_commitment,
                started.state.stored_last_bk_set_update_seq_no
            ),
            (c(S1), 40000)
        );
        assert_eq!(started.prover_bk_set.pubkeys().unwrap(), set(S1));
        assert_eq!(started.prover_bk_set.last_applied_update_seq_no, 40000);
        // Nothing is written.
        assert_eq!(std::fs::read(&paths.prover_bk_set_json).unwrap(), set_bytes);
        assert_eq!(
            std::fs::read(&paths.prover_state_json).unwrap(),
            mirror_bytes
        );
        assert!(other_files(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn resync_keeps_local_windows() {
        let h = world();
        // Seeded at 16384: the local layer-1 window still holds the seed root
        // ahead of the bundles, which the contract keeps only as
        // `storedPrevMaxLevelLayerHash`. Another relayer applied rotations
        // 50000 and 60000.
        let seed_root = [0x5e; 32];
        let mut chain = chain_state(49152, 60000, S2, c(S2), &[32768, 49152]);
        chain.genesis_prev_max_level_layer_hash = seed_root;
        let mut local = mirror(49152, 40000, S1, &[]);
        local.layer_windows[0].append(seed_root, 16384);
        for (hash, height) in chain.layer_windows[0]
            .data
            .iter()
            .zip(&chain.layer_windows[0].heights)
            .take(chain.layer_windows[0].data_len)
        {
            local.layer_windows[0].append(*hash, *height);
        }
        local.stored_last_seen_block_height = 777;
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        local
            .save(paths.prover_state_json.to_str().unwrap())
            .unwrap();
        file(S1, 40000)
            .save(paths.prover_bk_set_json.to_str().unwrap())
            .unwrap();
        let decision = decide_on(&local, &chain);
        assert_eq!(decision_kind(&decision), "resync");

        let started = startup_bk(
            decision,
            local.clone(),
            &chain,
            &paths,
            STRIDE,
            &h,
            &no_wait(),
        )
        .await
        .unwrap();
        // The local windows and block height, not the contract's.
        assert_eq!(started.state.layer_windows, local.layer_windows);
        assert_eq!(started.state.stored_last_seen_block_height, 777);
        // The contract's BK fields.
        assert_eq!(
            (
                started.state.stored_bk_set_commitment,
                started.state.stored_last_bk_set_update_seq_no
            ),
            (c(S2), 60000)
        );
        assert_eq!(started.seed_policy, SeedPolicy::Resume);
        assert_eq!(started.prover_bk_set.pubkeys().unwrap(), set(S2));
        assert_eq!(started.prover_bk_set.last_applied_update_seq_no, 60000);
        // Set and mirror are written; the old set is kept aside.
        assert_eq!(json(&load_mirror(&paths)), json(&started.state));
        assert_eq!(
            load_bk_file(&paths.prover_bk_set_json).unwrap().commitment,
            c(S2)
        );
        assert_eq!(backups(dir.path()), 1);
    }

    fn unbounded() -> BuildPolicy {
        BuildPolicy {
            data_attempts: 1,
            transient_budget: None,
            ..BuildPolicy::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn contract_lookup_is_retried_until_it_answers() {
        let h = FakeHistory {
            cursor_failures: AtomicUsize::new(3),
            ..world()
        };
        // The hold window of rotation 40000 needs the rotation below it.
        let chain = chain_state(16384, 40000, S1, c(S0), &[16384]);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        let t0 = Instant::now();
        let started = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &unbounded())
            .await
            .unwrap();
        assert_eq!(started.state.stored_last_bk_set_update_seq_no, 20000);
        assert_eq!(h.cursor_calls(), 4);
        // The policy's backoff: 2 s, 4 s, 8 s.
        assert_eq!(t0.elapsed(), Duration::from_secs(14));

        // A bounded policy gives up once its budget is spent, the last sleep
        // cut to fit, and writes nothing.
        let h = FakeHistory {
            cursor_failures: AtomicUsize::new(usize::MAX),
            ..world()
        };
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        let bounded = BuildPolicy {
            transient_budget: Some(Duration::from_secs(5)),
            ..unbounded()
        };
        let t0 = Instant::now();
        let e = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &bounded)
            .await
            .unwrap_err();
        assert!(matches!(e, BkStartupError::ContractFields(_)), "{e}");
        assert_eq!(t0.elapsed(), Duration::from_secs(5));
        assert_eq!(h.snapshot_calls(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn contract_lookup_fails_at_once_on_what_a_retry_cannot_cure() {
        // A server without the field: one question, no wait.
        let h = FakeHistory {
            cursor_unknown_field: true,
            ..world()
        };
        let chain = chain_state(16384, 40000, S1, c(S0), &[16384]);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        let t0 = Instant::now();
        let e = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &unbounded())
            .await
            .unwrap_err();
        assert!(matches!(e, BkStartupError::ContractFields(_)), "{e}");
        assert!(e.to_string().contains("does not know field"), "{e}");
        assert_eq!(h.cursor_calls(), 1);
        assert_eq!(t0.elapsed(), Duration::ZERO);

        // A hold window with no previous commitment on the contract.
        let h = world();
        let chain = chain_state(16384, 40000, S1, [0u8; 32], &[16384]);
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        let e = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &unbounded())
            .await
            .unwrap_err();
        assert!(matches!(e, BkStartupError::ContractFields(_)), "{e}");
        assert!(e.to_string().contains("storedPrevBkSetCommitment"), "{e}");
        assert_eq!(h.calls(), 0);
        assert_eq!(t0.elapsed(), Duration::ZERO);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn contract_mismatch_and_stop_write_nothing() {
        let h = world();
        // The leaves say S1 signs 49152; the contract is taken to hold S0.
        let chain = chain_state(49152, 40000, S0, c(S0), &[16384, 32768, 49152]);
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::under(dir.path());
        let local = load_mirror(&paths);
        let decision = decide_on(&local, &chain);
        assert_eq!(decision_kind(&decision), "resurrect");
        let e = startup_bk(decision, local, &chain, &paths, STRIDE, &h, &no_wait())
            .await
            .unwrap_err();
        assert!(matches!(e, BkStartupError::ContractMismatch { .. }), "{e}");

        let e = startup_bk(
            StartupDecision::Stop {
                reason: "windows diverge".into(),
            },
            load_mirror(&paths),
            &chain,
            &paths,
            STRIDE,
            &NoGraphql,
            &no_wait(),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("windows diverge"), "{e}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
