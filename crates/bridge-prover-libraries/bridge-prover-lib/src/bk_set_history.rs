//! Builds a block-keeper set at a height from the GraphQL history, retrying
//! the failures that retrying can cure.
//!
//! [`BkSetHistory`] is the IO surface (thread 0 of a gql-server);
//! [`build_bk_set_at`] makes one attempt through the pure replay in
//! [`crate::bk_set_replay`], [`build_bk_set_with_retry`] repeats it.

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use bridge_gql_fetcher::gql_client::{
    GqlClient, MalformedResponseError, UnknownFieldError, DEFAULT_THREAD_ID_HEX,
};
use tracing::warn;

use crate::bk_set_replay::{
    leaves_to_fetch, replay_to, rows_to_fetch, BkSetSnapshot, BkSetUpdateRow, BkTarget,
    BlockLeaves, FinalCheck, ReplayError, ReplayedSet,
};

/// Rows asked for per `bkSetUpdates` page.
const PAGE_SIZE: u32 = 100;

/// The field a gql-server must know to serve a snapshot.
const LATEST_BK_SET_FIELD: &str = "latestBkSet";

/// Read access to the block-keeper set history of thread 0.
#[allow(async_fn_in_trait)]
pub trait BkSetHistory {
    /// The set that signs the newest snapshot block; `None` while the server
    /// has no snapshot yet. An answer that cannot be read fails with a
    /// [`MalformedResponseError`].
    async fn latest_bk_set(&self) -> anyhow::Result<Option<BkSetSnapshot>>;
    /// Every `bkSetUpdates` row with a height in `[height_start, height_end]`,
    /// all pages.
    async fn bk_set_updates(
        &self,
        height_start: u64,
        height_end: u64,
    ) -> anyhow::Result<Vec<BkSetUpdateRow>>;
    /// Block-id-tree leaves of the block at `height`, `None` when the server
    /// has none.
    async fn block_leaves(&self, height: u64) -> anyhow::Result<Option<BlockLeaves>>;
    /// Height of the newest rotation at or below `height`, `None` when there
    /// is none.
    async fn last_update_at_or_below(&self, height: u64) -> anyhow::Result<Option<u64>>;
}

impl BkSetHistory for GqlClient {
    async fn latest_bk_set(&self) -> anyhow::Result<Option<BkSetSnapshot>> {
        Ok(self
            .query_latest_bk_set()
            .await?
            .map(|latest| BkSetSnapshot {
                anchor_height: latest.height,
                signers: latest.signers.into_iter().collect::<HashMap<_, _>>(),
            }))
    }

    async fn bk_set_updates(
        &self,
        height_start: u64,
        height_end: u64,
    ) -> anyhow::Result<Vec<BkSetUpdateRow>> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let (page, next) = self
                .query_bk_set_updates_range(
                    DEFAULT_THREAD_ID_HEX,
                    Some(height_start),
                    Some(height_end),
                    PAGE_SIZE,
                    after.as_deref(),
                )
                .await?;
            rows.extend(page.into_iter().map(|u| BkSetUpdateRow {
                height: u.height,
                bk_set_update_hex: u.bk_set_update_hex,
            }));
            match next {
                None => return Ok(rows),
                Some(cursor) => {
                    anyhow::ensure!(
                        after.as_deref() != Some(cursor.as_str()),
                        "bkSetUpdates returned the same page cursor twice"
                    );
                    after = Some(cursor);
                },
            }
        }
    }

    async fn block_leaves(&self, height: u64) -> anyhow::Result<Option<BlockLeaves>> {
        Ok(self
            .query_block_by_height(DEFAULT_THREAD_ID_HEX, height)
            .await?
            .block_merkle_tree_leaves)
    }

    async fn last_update_at_or_below(&self, height: u64) -> anyhow::Result<Option<u64>> {
        self.query_last_bk_set_update_at_or_below(DEFAULT_THREAD_ID_HEX, height)
            .await
    }
}

/// One attempt: snapshot, rows, leaves, replay.
pub async fn build_bk_set_at<H: BkSetHistory>(
    h: &H,
    target: &BkTarget,
) -> Result<ReplayedSet, ReplayError> {
    attempt(h, target, None, &mut None).await
}

/// [`build_bk_set_at`] that refuses a snapshot whose anchor is not above
/// `min_anchor` (reported as a transient failure) and records the anchor of
/// the snapshot it used in `used_anchor`. A snapshot that cannot be read is a
/// [`ReplayError::BadSnapshot`] and records no anchor.
async fn attempt<H: BkSetHistory>(
    h: &H,
    target: &BkTarget,
    min_anchor: Option<u64>,
    used_anchor: &mut Option<u64>,
) -> Result<ReplayedSet, ReplayError> {
    let snapshot = match h.latest_bk_set().await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Err(ReplayError::NoSnapshot),
        Err(e) => {
            let too_old = e
                .downcast_ref::<UnknownFieldError>()
                .is_some_and(|u| u.field == LATEST_BK_SET_FIELD);
            if too_old {
                return Err(ReplayError::ServerTooOld);
            }
            // Asking again gets the same answer: a data failure.
            if let Some(m) = e.downcast_ref::<MalformedResponseError>() {
                return Err(ReplayError::BadSnapshot {
                    reason: m.what.clone(),
                });
            }
            return Err(ReplayError::Query(e));
        },
    };

    if let Some(failed) = min_anchor {
        if snapshot.anchor_height <= failed {
            return Err(ReplayError::Query(anyhow::anyhow!(
                "no fresh snapshot yet: its anchor {} is not above {failed}, the anchor of the \
                 snapshot that failed its checks",
                snapshot.anchor_height
            )));
        }
    }
    *used_anchor = Some(snapshot.anchor_height);

    let rows = match rows_to_fetch(snapshot.anchor_height, target.height) {
        Some((from, to)) => h
            .bk_set_updates(from, to)
            .await
            .map_err(ReplayError::Query)?,
        None => Vec::new(),
    };

    let mut leaves = BTreeMap::new();
    for height in leaves_to_fetch(&rows, target) {
        match h.block_leaves(height).await.map_err(ReplayError::Query)? {
            Some(l) => {
                leaves.insert(height, l);
            },
            None => {
                return Err(ReplayError::MissingLeaves {
                    height,
                })
            },
        }
    }

    replay_to(&snapshot, &rows, &leaves, target)
}

/// How hard [`build_bk_set_with_retry`] tries.
#[derive(Clone, Copy, Debug)]
pub struct BuildPolicy {
    /// Data failures (a check against the leaves failed, or the snapshot
    /// could not be read) before giving up, counted over the whole call.
    pub data_attempts: u32,
    /// Bound on the total time slept on transient failures (the last sleep is
    /// cut short to fit), counted from the start or the last data failure;
    /// `None` retries them forever.
    pub transient_budget: Option<Duration>,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    pub backoff_multiplier: u32,
}

impl Default for BuildPolicy {
    fn default() -> Self {
        Self {
            data_attempts: 3,
            transient_budget: None,
            backoff_initial: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            backoff_multiplier: 2,
        }
    }
}

impl BuildPolicy {
    /// How long to sleep now, or `None` when the budget is spent. The sleep is
    /// cut to what is left of the budget.
    fn next_sleep(&self, waited: Duration, backoff: Duration) -> Option<Duration> {
        match self.transient_budget {
            None => Some(backoff),
            Some(budget) if waited >= budget => None,
            Some(budget) => Some(backoff.min(budget - waited)),
        }
    }

    fn next_backoff(&self, current: Duration) -> Duration {
        current
            .saturating_mul(self.backoff_multiplier)
            .min(self.backoff_max)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// `data_attempts` data failures; the message explains the last one.
    #[error("{}", data_failure(*attempts, last))]
    Data { attempts: u32, last: ReplayError },
    #[error("the block-keeper set queries kept failing for {waited:?}: {last}")]
    Transient { waited: Duration, last: ReplayError },
    #[error(
        "the GraphQL server does not know `latestBkSet`: needs gql-server >= 1.5.0 serving \
         blockchain.latestBkSet"
    )]
    NoLatestBkSetField,
}

/// What [`BuildError::Data`] tells the operator, by the kind of its last
/// failure.
fn data_failure(attempts: u32, last: &ReplayError) -> String {
    match last {
        ReplayError::BadSnapshot {
            ..
        } => format!(
            "the GraphQL server's `latestBkSet` answer could not be used on {attempts} attempts: \
             {last}. Check that the GraphQL endpoint serves the network the bridge follows and \
             that its gql-server version is one this release supports"
        ),
        ReplayError::BadRow {
            ..
        } => format!(
            "`bkSetUpdates` returned a row that cannot be replayed, on {attempts} snapshots: \
             {last}"
        ),
        _ => format!(
            "the block-keeper set failed its checks on {attempts} snapshots with a newer anchor \
             each: {last}. Either `bkSetUpdates` returned an incomplete range, or a change the \
             node applies implicitly (owner re-added under another index, overwrite, removal of \
             an absent index) is not in the rows"
        ),
    }
}

/// [`build_bk_set_at`] with retries: transient failures back off and repeat,
/// data failures repeat up to `policy.data_attempts`, each on a snapshot
/// anchored above the last one that was read and failed its checks, a server
/// without `latestBkSet` fails at once.
pub async fn build_bk_set_with_retry<H: BkSetHistory>(
    h: &H,
    target: &BkTarget,
    policy: &BuildPolicy,
) -> Result<ReplayedSet, BuildError> {
    let mut data_failures = 0u32;
    let mut waited = Duration::ZERO;
    let mut backoff = policy.backoff_initial;
    // After a data failure only a snapshot anchored above the failed one counts.
    let mut min_anchor: Option<u64> = None;
    loop {
        let mut used_anchor = None;
        let err = match attempt(h, target, min_anchor, &mut used_anchor).await {
            Ok(set) => return Ok(set),
            Err(ReplayError::ServerTooOld) => return Err(BuildError::NoLatestBkSetField),
            Err(e) => e,
        };
        if err.is_transient() {
            let Some(sleep) = policy.next_sleep(waited, backoff) else {
                return Err(BuildError::Transient {
                    waited,
                    last: err,
                });
            };
            warn!(error = %err, retry_in = ?sleep, "bk set build failed, will retry");
            tokio::time::sleep(sleep).await;
            waited = waited.saturating_add(sleep);
            backoff = policy.next_backoff(backoff);
        } else {
            // A snapshot that could not be read has no anchor to compare: the
            // bound of an earlier failure stays, and with none the next
            // attempt may take the same snapshot again.
            min_anchor = used_anchor.or(min_anchor);
            data_failures += 1;
            if data_failures >= policy.data_attempts {
                return Err(BuildError::Data {
                    attempts: data_failures,
                    last: err,
                });
            }
            warn!(
                error = %err,
                attempt = data_failures,
                "bk set failed its checks, retrying on a fresh snapshot"
            );
            waited = Duration::ZERO;
            backoff = policy.backoff_initial;
        }
    }
}

/// The target for a bridge seeded at `seed`: genesis checks block 1, any
/// other seed the set after the changes of the seed block.
pub fn seed_target(seed: u64) -> BkTarget {
    if seed == 0 {
        BkTarget {
            height: 0,
            check: FinalCheck::L2Of(1),
        }
    } else {
        BkTarget {
            height: seed,
            check: FinalCheck::L3Of(seed),
        }
    }
}

/// The set a relayer must hold, given the last seen block `last_seen`, the
/// newest rotation `last_bk_update` and the bundle `stride`.
pub fn bk_target(last_seen: u64, last_bk_update: u64, stride: u64) -> BkTarget {
    let held = last_bk_update != 0
        && stride != 0
        && (last_seen / stride + 1).saturating_mul(stride) <= last_bk_update;
    if held {
        BkTarget {
            height: last_bk_update - 1,
            check: FinalCheck::L2Of(last_bk_update),
        }
    } else if last_bk_update < last_seen {
        BkTarget {
            height: last_seen - 1,
            check: FinalCheck::L2Of(last_seen),
        }
    } else if last_bk_update == 0 {
        // `last_seen` is 0 as well.
        seed_target(0)
    } else {
        BkTarget {
            height: last_bk_update,
            check: FinalCheck::L3Of(last_bk_update),
        }
    }
}

/// The set at a bridge seed together with the newest rotation at or below it.
#[derive(Clone, Debug)]
pub struct GenesisBk {
    pub set: ReplayedSet,
    pub last_bk_set_update_seq_no: u64,
}

/// Build the set at `seed` and look up the newest rotation at or below it.
/// The rotation comes from its own query, not from the rows of the walk, which
/// need not reach it.
pub async fn genesis_bk_for_seed<H: BkSetHistory>(
    h: &H,
    seed: u64,
    policy: &BuildPolicy,
) -> anyhow::Result<GenesisBk> {
    let mut waited = Duration::ZERO;
    let mut backoff = policy.backoff_initial;
    let cursor = loop {
        match h.last_update_at_or_below(seed).await {
            Ok(found) => break found.unwrap_or(0),
            Err(e) if e.downcast_ref::<UnknownFieldError>().is_some() => return Err(e),
            Err(e) => {
                let Some(sleep) = policy.next_sleep(waited, backoff) else {
                    return Err(e.context(format!(
                        "last bk set update at or below {seed} kept failing for {waited:?}"
                    )));
                };
                warn!(
                    error = %e,
                    retry_in = ?sleep,
                    "last bk set update query failed, will retry"
                );
                tokio::time::sleep(sleep).await;
                waited = waited.saturating_add(sleep);
                backoff = policy.next_backoff(backoff);
            },
        }
    };

    // The cursor query spent part of the transient budget already.
    let remaining = BuildPolicy {
        transient_budget: policy.transient_budget.map(|b| b.saturating_sub(waited)),
        ..*policy
    };
    let set = build_bk_set_with_retry(h, &seed_target(seed), &remaining).await?;
    Ok(GenesisBk {
        set,
        last_bk_set_update_seq_no: cursor,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap, VecDeque},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use bridge_gql_fetcher::gql_client::{
        create_client, MalformedResponseError, UnknownFieldError, BLOCK_ID_TREE_LEAF_COUNT,
    };
    use halo2_base::halo2_proofs::halo2curves::{
        bls12_381::{Fr as BlsScalar, G1},
        group::Curve,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::Instant,
    };

    use super::*;
    use crate::bk_set_replay::commitment;

    fn key(seed: u64) -> Vec<u8> {
        (G1::generator() * BlsScalar::from(seed))
            .to_affine()
            .to_compressed_be()
            .to_vec()
    }

    fn set(entries: &[(u16, u64)]) -> HashMap<u16, Vec<u8>> {
        entries.iter().map(|(i, s)| (*i, key(*s))).collect()
    }

    fn c(entries: &[(u16, u64)]) -> [u8; 32] {
        commitment(&set(entries))
    }

    fn blob(variant: u32, index: u16, seed: u64) -> String {
        let point = (G1::generator() * BlsScalar::from(seed)).to_affine();
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&variant.to_le_bytes());
        buf.extend_from_slice(&index.to_le_bytes());
        buf.extend_from_slice(&96u64.to_le_bytes());
        buf.extend_from_slice(&point.to_uncompressed_be());
        hex::encode(buf)
    }

    fn row(height: u64, variant: u32, index: u16, seed: u64) -> BkSetUpdateRow {
        BkSetUpdateRow {
            height: Some(height),
            bk_set_update_hex: blob(variant, index, seed),
        }
    }

    fn leaves(l2: [u8; 32], l3: [u8; 32]) -> BlockLeaves {
        let mut l = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        l[2] = l2;
        l[3] = l3;
        l
    }

    fn snap(anchor_height: u64, entries: &[(u16, u64)]) -> BkSetSnapshot {
        BkSetSnapshot {
            anchor_height,
            signers: set(entries),
        }
    }

    // S0 --17000--> S1 --18000--> S2
    const S0: &[(u16, u64)] = &[(0, 1), (1, 2)];
    const S1: &[(u16, u64)] = &[(0, 1), (1, 2), (2, 3)];
    const S2: &[(u16, u64)] = &[(0, 1), (2, 3)];

    #[derive(Default)]
    struct FakeHistory {
        snapshots: Mutex<VecDeque<Option<BkSetSnapshot>>>,
        /// Until this much time has passed the server has no snapshot; then it
        /// answers with the given one.
        late_snapshot: Option<(Duration, BkSetSnapshot)>,
        started: Option<Instant>,
        unknown_field: bool,
        /// Calls of `latest_bk_set` (counted from 1) answered with an answer
        /// that cannot be read; the snapshot queue does not move on them.
        malformed_calls: Vec<usize>,
        rows: Vec<BkSetUpdateRow>,
        leaves: BTreeMap<u64, BlockLeaves>,
        last_le: BTreeMap<u64, u64>,
        /// `last_update_at_or_below` fails transiently this many times first.
        last_le_failures: AtomicUsize,
        last_le_always_fails: bool,
        last_le_unknown_field: bool,
        last_le_calls: AtomicUsize,
        latest_calls: AtomicUsize,
        updates_calls: AtomicUsize,
    }

    impl FakeHistory {
        fn with_snapshots(snapshots: Vec<Option<BkSetSnapshot>>) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into()),
                ..Default::default()
            }
        }

        fn latest_calls(&self) -> usize {
            self.latest_calls.load(Ordering::SeqCst)
        }

        /// Heights 17000 and 18000 rotate S0 -> S1 -> S2.
        fn chain(mut self) -> Self {
            self.rows = vec![row(17000, 0, 2, 3), row(18000, 1, 1, 2)];
            self.leaves.insert(16384, leaves(c(S0), c(S0)));
            self.leaves.insert(17000, leaves(c(S0), c(S1)));
            self.leaves.insert(18000, leaves(c(S1), c(S2)));
            self
        }
    }

    impl BkSetHistory for FakeHistory {
        async fn latest_bk_set(&self) -> anyhow::Result<Option<BkSetSnapshot>> {
            let call = self.latest_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.malformed_calls.contains(&call) {
                return Err(anyhow::Error::new(MalformedResponseError {
                    what: "latestBkSet.height is missing or not an unsigned integer".into(),
                }));
            }
            if self.unknown_field {
                return Err(anyhow::Error::new(UnknownFieldError {
                    field: "latestBkSet".into(),
                }));
            }
            if let (Some((after, late)), Some(started)) = (&self.late_snapshot, self.started) {
                if started.elapsed() >= *after {
                    return Ok(Some(late.clone()));
                }
                return Ok(None);
            }
            let mut q = self.snapshots.lock().unwrap();
            // The last entry repeats forever.
            Ok(if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().flatten()
            })
        }

        async fn bk_set_updates(
            &self,
            height_start: u64,
            height_end: u64,
        ) -> anyhow::Result<Vec<BkSetUpdateRow>> {
            self.updates_calls.fetch_add(1, Ordering::SeqCst);
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
            Ok(self.leaves.get(&height).copied())
        }

        async fn last_update_at_or_below(&self, height: u64) -> anyhow::Result<Option<u64>> {
            self.last_le_calls.fetch_add(1, Ordering::SeqCst);
            if self.last_le_unknown_field {
                return Err(anyhow::Error::new(UnknownFieldError {
                    field: "lastBkSetUpdate".into(),
                }));
            }
            if self.last_le_always_fails {
                anyhow::bail!("connection reset");
            }
            let left = self.last_le_failures.load(Ordering::SeqCst);
            if left > 0 {
                self.last_le_failures.store(left - 1, Ordering::SeqCst);
                anyhow::bail!("connection reset");
            }
            Ok(self.last_le.get(&height).copied())
        }
    }

    fn back_target() -> BkTarget {
        seed_target(16384)
    }

    #[tokio::test(start_paused = true)]
    async fn null_snapshot_retries_until_one_arrives() {
        let h = FakeHistory::with_snapshots(vec![None, None, Some(snap(19000, S2))]).chain();
        let out = build_bk_set_with_retry(&h, &back_target(), &BuildPolicy::default())
            .await
            .unwrap();
        assert_eq!(out.pubkeys, set(S0));
        assert_eq!(h.latest_calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn bad_snapshot_key_is_retried_not_fatal() {
        let mut bad = snap(19000, S2);
        bad.signers.insert(0, vec![0xff; 48]);
        let h = FakeHistory::with_snapshots(vec![Some(bad), Some(snap(19001, S2))]).chain();
        let out = build_bk_set_with_retry(&h, &back_target(), &BuildPolicy::default())
            .await
            .unwrap();
        assert_eq!(out.pubkeys, set(S0));
        assert_eq!(h.latest_calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn data_error_stops_after_three_attempts() {
        // Every attempt gets a newer snapshot, so every attempt replays.
        let mut h = FakeHistory::with_snapshots(vec![
            Some(snap(19000, S2)),
            Some(snap(19001, S2)),
            Some(snap(19002, S2)),
        ])
        .chain();
        h.rows.retain(|r| r.height != Some(17000));
        let e = build_bk_set_with_retry(&h, &back_target(), &BuildPolicy::default())
            .await
            .unwrap_err();
        assert!(
            matches!(e, BuildError::Data {
                attempts: 3,
                ..
            }),
            "{e}"
        );
        assert_eq!(h.latest_calls(), 3);
        assert_eq!(h.updates_calls.load(Ordering::SeqCst), 3);
        let msg = e.to_string();
        assert!(
            msg.contains("incomplete") && msg.contains("implicitly"),
            "{msg}"
        );
        assert!(msg.contains("height"), "{msg}");
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failure_does_not_reset_the_data_count() {
        // data, transient, data, data -> three data failures in total.
        let mut h = FakeHistory::with_snapshots(vec![
            Some(snap(19000, S2)),
            None,
            Some(snap(19001, S2)),
            Some(snap(19002, S2)),
        ])
        .chain();
        h.rows.retain(|r| r.height != Some(17000));
        let e = build_bk_set_with_retry(&h, &back_target(), &BuildPolicy::default())
            .await
            .unwrap_err();
        assert!(
            matches!(e, BuildError::Data {
                attempts: 3,
                ..
            }),
            "{e}"
        );
        assert_eq!(h.latest_calls(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_snapshot_after_a_data_failure_is_not_replayed() {
        let mut h = FakeHistory::with_snapshots(vec![
            Some(snap(19000, S2)),
            Some(snap(19000, S2)),
            Some(snap(19000, S2)),
            Some(snap(19001, S2)),
        ])
        .chain();
        h.rows.retain(|r| r.height != Some(17000));
        let p = BuildPolicy {
            data_attempts: 2,
            ..BuildPolicy::default()
        };
        let e = build_bk_set_with_retry(&h, &back_target(), &p)
            .await
            .unwrap_err();
        assert!(
            matches!(e, BuildError::Data {
                attempts: 2,
                ..
            }),
            "{e}"
        );
        // Four snapshots asked for, only the first and the newer one replayed.
        assert_eq!(h.latest_calls(), 4);
        assert_eq!(h.updates_calls.load(Ordering::SeqCst), 2);
    }

    /// An answer that cannot be read counts as a data failure and has no
    /// anchor: the bar of the failure before it stays.
    #[tokio::test(start_paused = true)]
    async fn unreadable_snapshot_is_a_data_failure_that_keeps_the_bar() {
        // 19000 fails its checks; call 2 cannot be read; 19000 again is
        // stale, not replayed; 19001 fails its checks.
        let mut h = FakeHistory::with_snapshots(vec![
            Some(snap(19000, S2)),
            Some(snap(19000, S2)),
            Some(snap(19001, S2)),
        ])
        .chain();
        h.malformed_calls = vec![2];
        h.rows.retain(|r| r.height != Some(17000));
        let (target, policy) = (back_target(), BuildPolicy::default());
        let run = build_bk_set_with_retry(&h, &target, &policy);
        let e = tokio::time::timeout(Duration::from_secs(3600), run)
            .await
            .expect("an unreadable snapshot must not be retried forever")
            .unwrap_err();
        assert!(
            matches!(e, BuildError::Data {
                attempts: 3,
                ..
            }),
            "{e}"
        );
        assert_eq!(h.latest_calls(), 4);
        assert_eq!(h.updates_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_transient_budget_gives_up() {
        let h = FakeHistory::with_snapshots(vec![None]).chain();
        let p = BuildPolicy {
            transient_budget: Some(Duration::from_secs(180)),
            ..BuildPolicy::default()
        };
        let started = Instant::now();
        let e = build_bk_set_with_retry(&h, &back_target(), &p)
            .await
            .unwrap_err();
        assert!(
            matches!(e, BuildError::Transient { waited, .. } if waited == Duration::from_secs(180)),
            "{e}"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(180));
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_after_a_minute_fits_the_budget() {
        let mut h = FakeHistory::default().chain();
        h.late_snapshot = Some((Duration::from_secs(70), snap(19000, S2)));
        h.started = Some(Instant::now());
        let p = BuildPolicy {
            transient_budget: Some(Duration::from_secs(180)),
            ..BuildPolicy::default()
        };
        let out = build_bk_set_with_retry(&h, &back_target(), &p)
            .await
            .unwrap();
        assert_eq!(out.pubkeys, set(S0));
    }

    #[tokio::test]
    async fn old_server_is_named_not_retried() {
        let h = FakeHistory {
            unknown_field: true,
            ..Default::default()
        };
        let e = build_bk_set_with_retry(&h, &back_target(), &BuildPolicy::default())
            .await
            .unwrap_err();
        assert!(matches!(e, BuildError::NoLatestBkSetField));
        assert!(e
            .to_string()
            .contains("needs gql-server >= 1.5.0 serving blockchain.latestBkSet"));
        assert_eq!(h.latest_calls(), 1);
    }

    #[tokio::test]
    async fn deploy_cursor_comes_from_its_own_query() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(19000, S2))]).chain();
        h.last_le.insert(16384, 15000);
        let g = genesis_bk_for_seed(&h, 16384, &BuildPolicy::default())
            .await
            .unwrap();
        // The backward walk only sees 17000 and 18000.
        assert_eq!(g.last_bk_set_update_seq_no, 15000);
        assert_eq!(g.set.pubkeys, set(S0));
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_query_retries_transient_failures() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(19000, S2))]).chain();
        h.last_le.insert(16384, 15000);
        h.last_le_failures = AtomicUsize::new(3);
        let g = genesis_bk_for_seed(&h, 16384, &BuildPolicy::default())
            .await
            .unwrap();
        assert_eq!(g.last_bk_set_update_seq_no, 15000);
        assert_eq!(h.last_le_calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_query_gives_up_when_the_budget_is_spent() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(19000, S2))]).chain();
        h.last_le_always_fails = true;
        let p = BuildPolicy {
            transient_budget: Some(Duration::from_secs(180)),
            ..BuildPolicy::default()
        };
        let started = Instant::now();
        let e = genesis_bk_for_seed(&h, 16384, &p).await.unwrap_err();
        assert_eq!(started.elapsed(), Duration::from_secs(180));
        assert!(e.to_string().contains("kept failing"), "{e}");
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_query_unknown_field_fails_at_once() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(19000, S2))]).chain();
        h.last_le_unknown_field = true;
        let started = Instant::now();
        let e = genesis_bk_for_seed(&h, 16384, &BuildPolicy::default())
            .await
            .unwrap_err();
        assert!(e.downcast_ref::<UnknownFieldError>().is_some(), "{e}");
        assert_eq!(h.last_le_calls.load(Ordering::SeqCst), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test]
    async fn deploy_cursor_survives_a_forward_walk() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(10000, S0))]);
        h.leaves.insert(16384, leaves(c(S0), c(S0)));
        h.last_le.insert(16384, 9000);
        let g = genesis_bk_for_seed(&h, 16384, &BuildPolicy::default())
            .await
            .unwrap();
        assert_eq!(g.last_bk_set_update_seq_no, 9000);
    }

    #[tokio::test]
    async fn deploy_cursor_is_zero_only_when_the_query_is_empty() {
        let mut h = FakeHistory::with_snapshots(vec![Some(snap(10000, S0))]);
        h.leaves.insert(16384, leaves(c(S0), c(S0)));
        let g = genesis_bk_for_seed(&h, 16384, &BuildPolicy::default())
            .await
            .unwrap();
        assert_eq!(g.last_bk_set_update_seq_no, 0);
    }

    #[test]
    fn target_table() {
        let s = 16384;
        assert_eq!(bk_target(16384, 40000, s), BkTarget {
            height: 39999,
            check: FinalCheck::L2Of(40000)
        });
        assert_eq!(bk_target(32768, 20000, s), BkTarget {
            height: 32767,
            check: FinalCheck::L2Of(32768)
        });
        assert_eq!(bk_target(16384, 20000, s), BkTarget {
            height: 20000,
            check: FinalCheck::L3Of(20000)
        });
        assert_eq!(bk_target(16384, 16384, s), BkTarget {
            height: 16384,
            check: FinalCheck::L3Of(16384)
        });
        assert_eq!(bk_target(0, 0, s), BkTarget {
            height: 0,
            check: FinalCheck::L2Of(1)
        });
        assert_eq!(bk_target(16384, 0, s), BkTarget {
            height: 16383,
            check: FinalCheck::L2Of(16384)
        });
    }

    #[test]
    fn seed_target_for_genesis_checks_block_one() {
        assert_eq!(seed_target(0), BkTarget {
            height: 0,
            check: FinalCheck::L2Of(1)
        });
        assert_eq!(seed_target(16384), BkTarget {
            height: 16384,
            check: FinalCheck::L3Of(16384)
        });
    }

    /// A GraphQL server on a local port that answers every request with
    /// `body`: its URL, the number of requests answered, and its task.
    async fn serve(body: &'static str) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let counter = counter.clone();
                tokio::spawn(async move {
                    // Read the headers and the whole body, then answer.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(split) = text.find("\r\n\r\n") {
                            let want = text[..split]
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if buf.len() >= split + 4 + want {
                                break;
                            }
                        }
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
                         {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (url, requests, server)
    }

    /// The unknown-field answer of an old gql-server survives the real
    /// adapter and is recognised after one request.
    #[tokio::test]
    async fn old_server_over_http_is_named_after_one_request() {
        const BODY: &str = r#"{"data":null,"errors":[{"message":"Unknown field \"latestBkSet\" on type \"BlockchainQuery\".","locations":[{"line":1,"column":16}]}]}"#;
        let (url, requests, server) = serve(BODY).await;
        let client = create_client(&url).unwrap();
        let e = build_bk_set_with_retry(&client, &seed_target(16384), &BuildPolicy::default())
            .await
            .unwrap_err();
        server.abort();
        assert!(matches!(e, BuildError::NoLatestBkSetField), "{e}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    /// A `latestBkSet` that cannot be read, here a key with a `0x` prefix,
    /// fails the build after three requests instead of being retried as a
    /// failed query forever.
    #[tokio::test]
    async fn unreadable_snapshot_over_http_fails_after_three_requests() {
        const BODY: &str = r#"{"data":{"blockchain":{"latestBkSet":{"thread_id":"00000000000000000000000000000000000000000000000000000000000000000000","seq_no":19000,"block_id":"bb","height":19000,"chain_order":"x","updated_at":1,"signers":[{"signer_index":0,"pubkey":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}}}}"#;
        let (url, requests, server) = serve(BODY).await;
        let client = create_client(&url).unwrap();
        let policy = BuildPolicy {
            transient_budget: None,
            ..BuildPolicy::default()
        };
        let target = seed_target(16384);
        let run = build_bk_set_with_retry(&client, &target, &policy);
        let e = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("an unreadable snapshot must not be retried forever")
            .unwrap_err();
        server.abort();
        assert!(
            matches!(e, BuildError::Data {
                attempts: 3,
                ..
            }),
            "{e}"
        );
        let msg = e.to_string();
        assert!(
            msg.contains("`latestBkSet` answer could not be used")
                && msg.contains("latestBkSet.signers[0].pubkey has 98 hex chars"),
            "{msg}"
        );
        assert!(!msg.contains("incomplete range"), "{msg}");
        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }

    /// The explanation follows the last failure: only a set that fails its
    /// checks against the leaves points at the rows.
    #[test]
    fn data_error_text_follows_the_last_failure() {
        let data = |last| {
            BuildError::Data {
                attempts: 3,
                last,
            }
            .to_string()
        };
        let snapshot = data(ReplayError::BadSnapshot {
            reason: "latestBkSet.signers[0].pubkey has 98 hex chars, expected 96".into(),
        });
        assert!(
            snapshot.contains("`latestBkSet` answer could not be used on 3 attempts")
                && snapshot.contains("has 98 hex chars")
                && snapshot.contains("gql-server version"),
            "{snapshot}"
        );
        assert!(
            !snapshot.contains("incomplete range")
                && !snapshot.contains("failed its checks")
                && !snapshot.contains("update row"),
            "{snapshot}"
        );

        let row = data(ReplayError::BadRow {
            reason: "row has no height".into(),
        });
        assert!(
            row.contains("`bkSetUpdates` returned a row") && !row.contains("incomplete range"),
            "{row}"
        );

        let mismatch = data(ReplayError::Mismatch {
            height: 18_000,
            step: "final",
            expected: [0; 32],
            actual: [1; 32],
        });
        assert!(
            mismatch.contains("failed its checks on 3 snapshots")
                && mismatch.contains("incomplete range")
                && mismatch.contains("implicitly"),
            "{mismatch}"
        );
        let undo = data(ReplayError::Undo {
            height: 18_000,
            signer_index: 1,
            reason: "absent",
        });
        assert!(undo.contains("incomplete range"), "{undo}");
    }
}
