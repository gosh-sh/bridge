//! Stateful BK-set change detector layered on top of [`BkSetClient`].
//!
//! The relayer needs to know when the Acki Nacki Block-Keeper committee
//! has rotated so that it can:
//!
//! 1. Detect divergence between the bridge's `storedBkSetCommitment` and the
//!    live node — refuse to submit `verifyBlock` proofs that were generated
//!    against a stale committee.
//! 2. Trigger Circuit 3 (BK-set rotation) proof generation + submission of the
//!    resulting `rotateBkSet` transaction, once Phase 1.C / Phase 3.4 land.
//!
//! `BkSetTracker` polls `/v2/bk_set_update`, caches the last observed
//! snapshot, and surfaces structured `BkSetChange` events on every
//! `poll()`. Diff is computed over the **`signer_index → pubkey`**
//! map — the same surface the bridge ultimately commits to via the
//! Poseidon commitment.
//!
//! Computing the Poseidon commitment itself is **out of scope here**:
//! that requires arkworks BN254 + the partner's hash chip and lives
//! in `bridge-prover-orchestrator`. This module limits itself to
//! membership change detection, which is `serde_json`-only.
//!
//! ## Phase 5.2 wiring sketch
//!
//! ```text
//! loop {
//!     match tracker.poll().await? {
//!         BkSetChange::Unchanged { .. } => { /* normal path */ }
//!         BkSetChange::MembershipChanged(delta) => {
//!             orchestrator.regenerate_rotation_proof(delta).await?;
//!             bridge.submit_rotation(...).await?;
//!         }
//!         BkSetChange::FirstObservation { .. } => { /* warm cache */ }
//!     }
//! }
//! ```
//!
//! The `seq_no` field of the snapshot is intentionally NOT included in the
//! "did membership change?" decision — Acki Nacki bumps `seq_no` on every
//! processed block, so naïve seq_no comparison would fire `Changed`
//! constantly. The tracker only flags a change when the membership
//! set (signer_index → pubkey) actually differs.

use std::collections::{BTreeMap, BTreeSet};

use tracing::{debug, info};

use crate::{
    bk_set_client::{BkSetClient, BkSetUpdateResponse, BkUpdateEntry, BLS_PUBKEY_LEN},
    error::{AckiNackiError, Result},
};

/// Canonical, hash-friendly view of a single observation of the BK set.
///
/// We deliberately use `BTreeMap` (not `HashMap`) so equality + ordering
/// are deterministic, which makes diffs and tests stable. The cost over
/// `HashMap` is negligible — the BK set is sized in the thousands at most.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BkSetSnapshot {
    /// `/v2/bk_set_update.seq_no` at the time of the observation.
    pub observed_seq_no: u64,
    /// `signer_index → 48-byte BLS12-381 G1 compressed pubkey`. This is
    /// the same shape `bridge-prover-orchestrator::generate_fallback_proof`
    /// and the bridge's `storedBkSetCommitment` ultimately commit to.
    pub current: BTreeMap<u32, [u8; BLS_PUBKEY_LEN]>,
    /// Future epoch's BK set (look-ahead window). Empty most of the time.
    pub future: BTreeMap<u32, [u8; BLS_PUBKEY_LEN]>,
}

impl BkSetSnapshot {
    /// Build a snapshot from a `/v2/bk_set_update` response.
    ///
    /// Returns `InvalidLength` if any `pubkey` doesn't decode to 48 bytes
    /// and `InvalidTransaction` on duplicate signer indices (node bug).
    pub fn from_update(update: BkSetUpdateResponse) -> Result<Self> {
        Ok(Self {
            observed_seq_no: update.seq_no,
            current: entries_to_map(&update.current, "current")?,
            future: entries_to_map(&update.future, "future")?,
        })
    }

    /// Number of active BKs in the current committee.
    pub fn current_size(&self) -> usize {
        self.current.len()
    }

    /// Hash-friendly canonical bytes: little-endian `signer_index` (4 bytes)
    /// followed by the 48-byte pubkey, repeated for every entry in
    /// `current` sorted by `signer_index`. Useful for tests + lightweight
    /// fingerprinting; this is **not** the on-chain Poseidon commitment.
    pub fn canonical_bytes_current(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.current.len() * (4 + BLS_PUBKEY_LEN));
        for (idx, pk) in &self.current {
            out.extend_from_slice(&idx.to_le_bytes());
            out.extend_from_slice(pk);
        }
        out
    }
}

/// Structured diff between two snapshots.
///
/// `pubkey_mutations` flags the (rare and concerning) case where the same
/// `signer_index` reappears with a different BLS pubkey — semantically
/// that would mean the node rebound an enrolment slot, which should not
/// happen. Surface it loudly so an operator can investigate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MembershipDelta {
    /// `signer_index` values present in `new.current` but not in `old.current`.
    pub added: BTreeSet<u32>,
    /// `signer_index` values present in `old.current` but not in `new.current`.
    pub removed: BTreeSet<u32>,
    /// `signer_index` values where the BLS pubkey changed between the two
    /// snapshots — shouldn't happen in practice. Surface for investigation.
    pub pubkey_mutations: BTreeSet<u32>,
}

impl MembershipDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.pubkey_mutations.is_empty()
    }
}

/// Event surfaced by [`BkSetTracker::poll`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BkSetChange {
    /// First successful poll since the tracker was constructed; there is
    /// no prior snapshot to diff against. The relayer should treat this
    /// as "warm cache" — record the commitment off-chain, compare it
    /// against the bridge's `storedBkSetCommitment` if relevant, and
    /// continue. Carries the new snapshot for convenience.
    FirstObservation { snapshot: BkSetSnapshot },
    /// New snapshot is byte-for-byte equivalent to the cached one in the
    /// **membership** sense (signer_index + pubkey identical for every
    /// active BK; future may differ — see `future_changed`). `observed_seq_no`
    /// will typically differ.
    Unchanged {
        observed_seq_no: u64,
        future_changed: bool,
    },
    /// Active committee membership changed; a Circuit 3 rotation proof
    /// will be required to advance the bridge's `storedBkSetCommitment`.
    MembershipChanged {
        old_seq_no: u64,
        new_seq_no: u64,
        delta: MembershipDelta,
    },
}

/// Stateful poller. Cheap to construct, holds a `BkSetClient` (itself
/// `Clone`) and the last observed snapshot.
#[derive(Clone, Debug)]
pub struct BkSetTracker {
    client: BkSetClient,
    last: Option<BkSetSnapshot>,
}

impl BkSetTracker {
    pub fn new(client: BkSetClient) -> Self {
        Self { client, last: None }
    }

    /// Take one observation. On the first call returns
    /// `BkSetChange::FirstObservation`; subsequent calls return either
    /// `Unchanged` or `MembershipChanged`.
    ///
    /// Internal cache is updated on every successful poll, so the
    /// tracker is ready to detect the **next** change against the
    /// freshly-fetched snapshot. A poll that returns an error leaves
    /// the cache untouched.
    pub async fn poll(&mut self) -> Result<BkSetChange> {
        let update = self.client.fetch_bk_set_update().await?;
        let new_snapshot = BkSetSnapshot::from_update(update)?;
        let event = match &self.last {
            None => {
                info!(
                    seq_no = new_snapshot.observed_seq_no,
                    bk_count = new_snapshot.current_size(),
                    "BkSetTracker: first observation"
                );
                BkSetChange::FirstObservation {
                    snapshot: new_snapshot.clone(),
                }
            },
            Some(prev) => diff_snapshots(prev, &new_snapshot),
        };
        self.last = Some(new_snapshot);
        Ok(event)
    }

    /// Last observed snapshot, if any.
    pub fn latest(&self) -> Option<&BkSetSnapshot> {
        self.last.as_ref()
    }

    /// Reset the cache. After this, the next `poll()` will return
    /// `FirstObservation`. Useful when the relayer reconciles against
    /// fresh on-chain state at startup.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

fn diff_snapshots(prev: &BkSetSnapshot, new: &BkSetSnapshot) -> BkSetChange {
    let prev_keys: BTreeSet<u32> = prev.current.keys().copied().collect();
    let new_keys: BTreeSet<u32> = new.current.keys().copied().collect();

    let added: BTreeSet<u32> = new_keys.difference(&prev_keys).copied().collect();
    let removed: BTreeSet<u32> = prev_keys.difference(&new_keys).copied().collect();
    let mut mutations = BTreeSet::new();
    for idx in prev_keys.intersection(&new_keys) {
        if prev.current[idx] != new.current[idx] {
            mutations.insert(*idx);
        }
    }

    if added.is_empty() && removed.is_empty() && mutations.is_empty() {
        let future_changed = prev.future != new.future;
        debug!(
            seq_no = new.observed_seq_no,
            future_changed, "BkSetTracker: membership unchanged"
        );
        BkSetChange::Unchanged {
            observed_seq_no: new.observed_seq_no,
            future_changed,
        }
    } else {
        let delta = MembershipDelta {
            added,
            removed,
            pubkey_mutations: mutations,
        };
        info!(
            old_seq_no = prev.observed_seq_no,
            new_seq_no = new.observed_seq_no,
            added = delta.added.len(),
            removed = delta.removed.len(),
            mutations = delta.pubkey_mutations.len(),
            "BkSetTracker: membership changed"
        );
        BkSetChange::MembershipChanged {
            old_seq_no: prev.observed_seq_no,
            new_seq_no: new.observed_seq_no,
            delta,
        }
    }
}

fn entries_to_map(
    entries: &[BkUpdateEntry],
    field: &'static str,
) -> Result<BTreeMap<u32, [u8; BLS_PUBKEY_LEN]>> {
    let mut out = BTreeMap::new();
    for e in entries {
        let raw = hex::decode(&e.pubkey).map_err(|err| {
            AckiNackiError::JsonParse(format!(
                "{}: hex-decode failed for entry signer_index={}: {err}",
                field, e.signer_index,
            ))
        })?;
        if raw.len() != BLS_PUBKEY_LEN {
            return Err(AckiNackiError::InvalidLength {
                field: "bk_set_update.pubkey",
                expected: BLS_PUBKEY_LEN,
                actual: raw.len(),
            });
        }
        let mut buf = [0u8; BLS_PUBKEY_LEN];
        buf.copy_from_slice(&raw);
        if out.insert(e.signer_index, buf).is_some() {
            return Err(AckiNackiError::InvalidTransaction(format!(
                "duplicate signer_index {} in /v2/bk_set_update.{field}",
                e.signer_index,
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bk_set_client::{BkSetUpdateResponse, BkUpdateEntry};

    fn pk(byte: u8) -> String {
        // 48 bytes = 96 hex chars, all the same byte.
        let b = [byte; BLS_PUBKEY_LEN];
        hex::encode(b)
    }

    fn entry(signer_index: u32, pubkey_byte: u8) -> BkUpdateEntry {
        BkUpdateEntry {
            pubkey: pk(pubkey_byte),
            epoch_finish_seq_no: 1,
            wait_step: 180,
            status: "Active".to_string(),
            address: "0:0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            stake: "0".to_string(),
            owner_address: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            signer_index,
            owner_pubkey: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            protocol_version_support: "".to_string(),
        }
    }

    fn update(seq_no: u64, current: Vec<BkUpdateEntry>) -> BkSetUpdateResponse {
        BkSetUpdateResponse {
            seq_no,
            current,
            future: vec![],
        }
    }

    #[test]
    fn snapshot_from_update_builds_btreemap() {
        let resp = update(42, vec![entry(7, 0x07), entry(3, 0x03), entry(11, 0x0b)]);
        let snap = BkSetSnapshot::from_update(resp).unwrap();
        assert_eq!(snap.observed_seq_no, 42);
        assert_eq!(snap.current_size(), 3);
        let keys: Vec<u32> = snap.current.keys().copied().collect();
        assert_eq!(keys, vec![3, 7, 11], "BTreeMap iterates in sorted order");
    }

    #[test]
    fn snapshot_from_update_rejects_duplicate_signer_index() {
        let resp = update(42, vec![entry(7, 0x07), entry(7, 0x09)]);
        let err = BkSetSnapshot::from_update(resp).unwrap_err();
        assert!(matches!(err, AckiNackiError::InvalidTransaction(_)));
    }

    #[test]
    fn snapshot_from_update_rejects_short_pubkey() {
        let mut bad = entry(7, 0x07);
        bad.pubkey = "aa".to_string();
        let err = BkSetSnapshot::from_update(update(42, vec![bad])).unwrap_err();
        assert!(matches!(
            err,
            AckiNackiError::InvalidLength {
                expected: BLS_PUBKEY_LEN,
                actual: 1,
                ..
            }
        ));
    }

    #[test]
    fn diff_detects_unchanged() {
        let a =
            BkSetSnapshot::from_update(update(10, vec![entry(1, 0x01), entry(2, 0x02)])).unwrap();
        let b =
            BkSetSnapshot::from_update(update(11, vec![entry(2, 0x02), entry(1, 0x01)])).unwrap();
        match diff_snapshots(&a, &b) {
            BkSetChange::Unchanged {
                observed_seq_no,
                future_changed,
            } => {
                assert_eq!(observed_seq_no, 11);
                assert!(!future_changed);
            },
            other => panic!("expected Unchanged, got {other:?}"),
        }
    }

    #[test]
    fn diff_detects_add_and_remove() {
        let a =
            BkSetSnapshot::from_update(update(10, vec![entry(1, 0x01), entry(2, 0x02)])).unwrap();
        let b =
            BkSetSnapshot::from_update(update(20, vec![entry(2, 0x02), entry(3, 0x03)])).unwrap();
        match diff_snapshots(&a, &b) {
            BkSetChange::MembershipChanged {
                old_seq_no,
                new_seq_no,
                delta,
            } => {
                assert_eq!(old_seq_no, 10);
                assert_eq!(new_seq_no, 20);
                assert_eq!(delta.added, BTreeSet::from([3]));
                assert_eq!(delta.removed, BTreeSet::from([1]));
                assert!(delta.pubkey_mutations.is_empty());
            },
            other => panic!("expected MembershipChanged, got {other:?}"),
        }
    }

    #[test]
    fn diff_detects_pubkey_mutation() {
        let a =
            BkSetSnapshot::from_update(update(10, vec![entry(5, 0x05), entry(7, 0x07)])).unwrap();
        let b =
            BkSetSnapshot::from_update(update(20, vec![entry(5, 0xaa), entry(7, 0x07)])).unwrap();
        match diff_snapshots(&a, &b) {
            BkSetChange::MembershipChanged { delta, .. } => {
                assert!(delta.added.is_empty());
                assert!(delta.removed.is_empty());
                assert_eq!(delta.pubkey_mutations, BTreeSet::from([5]));
            },
            other => panic!("expected MembershipChanged, got {other:?}"),
        }
    }

    #[test]
    fn diff_flags_future_change_when_current_is_unchanged() {
        let mut a_resp = update(10, vec![entry(1, 0x01)]);
        a_resp.future = vec![entry(99, 0x99)];
        let mut b_resp = update(11, vec![entry(1, 0x01)]);
        b_resp.future = vec![]; // future emptied — common when an upcoming BK got dropped
        let a = BkSetSnapshot::from_update(a_resp).unwrap();
        let b = BkSetSnapshot::from_update(b_resp).unwrap();
        match diff_snapshots(&a, &b) {
            BkSetChange::Unchanged { future_changed, .. } => {
                assert!(future_changed);
            },
            other => panic!("expected Unchanged with future_changed=true, got {other:?}"),
        }
    }

    #[test]
    fn canonical_bytes_is_deterministic_and_sorted() {
        let snap = BkSetSnapshot::from_update(update(
            1,
            vec![entry(11, 0x0b), entry(3, 0x03), entry(7, 0x07)],
        ))
        .unwrap();
        let bytes = snap.canonical_bytes_current();
        // 3 entries * (4 bytes idx + 48 bytes pubkey) = 156 bytes.
        assert_eq!(bytes.len(), 3 * (4 + BLS_PUBKEY_LEN));
        // First 4 bytes are signer_index 3 in LE.
        assert_eq!(&bytes[0..4], &3u32.to_le_bytes());
        // Bytes 52..56 are signer_index 7 in LE.
        assert_eq!(&bytes[52..56], &7u32.to_le_bytes());
        // Bytes 104..108 are signer_index 11 in LE.
        assert_eq!(&bytes[104..108], &11u32.to_le_bytes());
    }
}
