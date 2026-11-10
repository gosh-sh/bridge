//! Pure replay of a block-keeper set between a snapshot and a target height.
//!
//! The snapshot is the set that signs the anchor block `A` (leaf `[2]` of `A`).
//! Rows are the `bkSetUpdates` between `A` and the target `T`. Every row at
//! height `h` is checked against the block-id-tree leaves of `h`: leaf `[2]`
//! is the commitment of the set that signs `h`, leaf `[3]` the commitment of
//! the set after the changes of `h`. Hence `L3(h) == L2(h + 1)`.
//!
//! Going forward (`T >= A`) the rows in `[A, T]` are applied; going backward
//! (`T < A`) the rows in `(T, A)` are undone, last row first. The result is
//! finally compared with the leaf named by [`FinalCheck`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bridge_gql_fetcher::{
    bk_set_fetcher::{normalize_bk_set_pubkeys, parse_bk_set_changes_pub},
    gql_client::BLOCK_ID_TREE_LEAF_COUNT,
};
use bridge_poseidon as poseidon;
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;

/// The 16 leaves of a block-id tree.
pub type BlockLeaves = [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];

const LEAF_L2: usize = 2;
const LEAF_L3: usize = 3;

const VARIANT_ADDED: u32 = 0;
const VARIANT_REMOVED: u32 = 1;

/// One change of a block's `bk_set_update` blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BkChange {
    Added(u16, Vec<u8>),
    Removed(u16, Vec<u8>),
    /// Any other change kind; it does not touch the set.
    Other,
}

/// What the replayed set is compared with at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalCheck {
    /// Leaf `[3]` of this block.
    L3Of(u64),
    /// Leaf `[2]` of this block.
    L2Of(u64),
}

impl FinalCheck {
    fn height(self) -> u64 {
        match self {
            FinalCheck::L3Of(h) | FinalCheck::L2Of(h) => h,
        }
    }

    fn leaf_index(self) -> usize {
        match self {
            FinalCheck::L3Of(_) => LEAF_L3,
            FinalCheck::L2Of(_) => LEAF_L2,
        }
    }
}

/// The height to build the set for and the leaf that vouches for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BkTarget {
    pub height: u64,
    pub check: FinalCheck,
}

/// The set that signs the anchor block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BkSetSnapshot {
    pub anchor_height: u64,
    pub signers: HashMap<u16, Vec<u8>>,
}

/// One `bkSetUpdates` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BkSetUpdateRow {
    pub height: Option<u64>,
    pub bk_set_update_hex: String,
}

/// A set that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedSet {
    pub pubkeys: HashMap<u16, Vec<u8>>,
    pub commitment: [u8; 32],
    pub height: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(
        "bk set commitment mismatch at height {height}, step {step}: expected {}, got {}",
        hex::encode(expected),
        hex::encode(actual)
    )]
    Mismatch {
        height: u64,
        step: &'static str,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    #[error("cannot undo bk set change at height {height}, signer {signer_index}: {reason}")]
    Undo {
        height: u64,
        signer_index: u16,
        reason: &'static str,
    },
    #[error("bad bk set update row: {reason}")]
    BadRow { reason: String },
    #[error("block-id-tree leaves of height {height} are not available")]
    MissingLeaves { height: u64 },
    #[error("the server returned no latestBkSet snapshot")]
    NoSnapshot,
    /// The server does not know `latestBkSet`; retrying cannot help.
    #[error("the server does not support latestBkSet")]
    ServerTooOld,
    #[error("bk set query failed: {0}")]
    Query(#[source] anyhow::Error),
}

impl ReplayError {
    /// Whether asking again later may succeed.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ReplayError::MissingLeaves { .. } | ReplayError::NoSnapshot | ReplayError::Query(_)
        )
    }
}

fn bad_row(reason: impl Into<String>) -> ReplayError {
    ReplayError::BadRow {
        reason: reason.into(),
    }
}

/// Decode a hex `bk_set_update` blob. A blob with no changes is a bad row.
pub fn parse_changes(blob_hex: &str) -> Result<Vec<BkChange>, ReplayError> {
    let blob = hex::decode(blob_hex).map_err(|e| bad_row(format!("blob is not hex: {e}")))?;
    let changes: Vec<BkChange> = parse_bk_set_changes_pub(&blob)
        .into_iter()
        .map(|(variant, index, key)| match variant {
            VARIANT_ADDED => BkChange::Added(index, key),
            VARIANT_REMOVED => BkChange::Removed(index, key),
            _ => BkChange::Other,
        })
        .collect();
    if changes.is_empty() {
        return Err(bad_row("blob holds no changes"));
    }
    Ok(changes)
}

/// Canonical form of a key: 48-byte big-endian compressed. 48-byte input is
/// decoded as big-endian, then little-endian compressed (as the commitment
/// does) and re-encoded; 96-byte input goes through `normalize_bk_set_pubkeys`.
/// `None` for any other length or a key that is not a curve point.
fn canonical_key(index: u16, key: &[u8]) -> Option<Vec<u8>> {
    match key.len() {
        48 => {
            let bytes: [u8; 48] = key.try_into().ok()?;
            let be = G1Affine::from_compressed_be(&bytes);
            if bool::from(be.is_some()) {
                return Some(be.unwrap().to_compressed_be().to_vec());
            }
            let le = G1Affine::from_compressed_le(&bytes);
            if bool::from(le.is_some()) {
                return Some(le.unwrap().to_compressed_be().to_vec());
            }
            None
        },
        96 => {
            let mut one = HashMap::new();
            one.insert(index, key.to_vec());
            normalize_bk_set_pubkeys(one).ok()?.remove(&index)
        },
        _ => None,
    }
}

fn compressed(index: u16, key: &[u8]) -> Result<Vec<u8>, ReplayError> {
    canonical_key(index, key).ok_or_else(|| bad_row(format!("signer {index} has an invalid key")))
}

/// Canonicalise every snapshot key; a bad key is a data error, never a panic.
fn canonical_snapshot(
    signers: &HashMap<u16, Vec<u8>>,
) -> Result<HashMap<u16, Vec<u8>>, ReplayError> {
    signers
        .iter()
        .map(|(index, key)| {
            canonical_key(*index, key)
                .map(|k| (*index, k))
                .ok_or_else(|| bad_row(format!("snapshot signer {index} has an invalid key")))
        })
        .collect()
}

/// Apply a block's changes the way the node does: every removal first, then
/// every addition.
pub fn apply_block_changes(
    set: &mut HashMap<u16, Vec<u8>>,
    changes: &[BkChange],
) -> Result<(), ReplayError> {
    for change in changes {
        if let BkChange::Removed(index, _) = change {
            set.remove(index);
        }
    }
    for change in changes {
        if let BkChange::Added(index, key) = change {
            set.insert(*index, compressed(*index, key)?);
        }
    }
    Ok(())
}

/// Mirror of [`apply_block_changes`]: take back every addition, then every
/// removal. `height` only labels errors.
fn undo_at(
    height: u64,
    set: &mut HashMap<u16, Vec<u8>>,
    changes: &[BkChange],
) -> Result<(), ReplayError> {
    for change in changes {
        if let BkChange::Added(index, key) = change {
            let key = compressed(*index, key)?;
            match set.get(index) {
                Some(current) if *current == key => {
                    set.remove(index);
                },
                Some(_) => {
                    return Err(ReplayError::Undo {
                        height,
                        signer_index: *index,
                        reason: "added key differs",
                    })
                },
                None => {
                    return Err(ReplayError::Undo {
                        height,
                        signer_index: *index,
                        reason: "added key is absent",
                    })
                },
            }
        }
    }
    for change in changes {
        if let BkChange::Removed(index, key) = change {
            if set.contains_key(index) {
                return Err(ReplayError::Undo {
                    height,
                    signer_index: *index,
                    reason: "index occupied",
                });
            }
            set.insert(*index, compressed(*index, key)?);
        }
    }
    Ok(())
}

/// Take back a block's changes, in the reverse order of
/// [`apply_block_changes`]. Errors carry height 0; [`replay_to`] reports the
/// real one.
pub fn undo_block_changes(
    set: &mut HashMap<u16, Vec<u8>>,
    changes: &[BkChange],
) -> Result<(), ReplayError> {
    undo_at(0, set, changes)
}

/// Poseidon commitment of a set of 48-byte compressed keys.
pub fn commitment(set: &HashMap<u16, Vec<u8>>) -> [u8; 32] {
    poseidon::compute_bk_set_poseidon(set).1
}

/// The inclusive height range the rows must lie in, or `None` when no row can.
pub fn rows_to_fetch(anchor_height: u64, target_height: u64) -> Option<(u64, u64)> {
    if target_height >= anchor_height {
        Some((anchor_height, target_height))
    } else {
        let from = target_height + 1;
        let to = anchor_height - 1;
        (from <= to).then_some((from, to))
    }
}

/// Heights whose leaves [`replay_to`] reads: those of the rows and of the
/// final check, nothing else.
pub fn leaves_to_fetch(rows: &[BkSetUpdateRow], target: &BkTarget) -> Vec<u64> {
    let mut heights: BTreeSet<u64> = rows.iter().filter_map(|r| r.height).collect();
    heights.insert(target.check.height());
    heights.into_iter().collect()
}

fn leaves_of(
    leaves: &BTreeMap<u64, BlockLeaves>,
    height: u64,
) -> Result<&BlockLeaves, ReplayError> {
    leaves.get(&height).ok_or(ReplayError::MissingLeaves {
        height,
    })
}

fn expect(
    height: u64,
    step: &'static str,
    expected: [u8; 32],
    set: &HashMap<u16, Vec<u8>>,
) -> Result<(), ReplayError> {
    let actual = commitment(set);
    if actual == expected {
        Ok(())
    } else {
        Err(ReplayError::Mismatch {
            height,
            step,
            expected,
            actual,
        })
    }
}

/// Check that heights are present, strictly increasing and inside the range;
/// return them with their parsed changes.
fn checked_rows(
    snapshot: &BkSetSnapshot,
    rows: &[BkSetUpdateRow],
    target_height: u64,
) -> Result<Vec<(u64, Vec<BkChange>)>, ReplayError> {
    let range = rows_to_fetch(snapshot.anchor_height, target_height);
    let mut out = Vec::with_capacity(rows.len());
    let mut previous: Option<u64> = None;
    for row in rows {
        let height = row.height.ok_or_else(|| bad_row("row has no height"))?;
        match range {
            Some((from, to)) if (from..=to).contains(&height) => {},
            _ => {
                return Err(bad_row(format!(
                    "row at height {height} is outside the range for anchor {} and target \
                     {target_height}",
                    snapshot.anchor_height
                )))
            },
        }
        if previous.is_some_and(|p| height <= p) {
            return Err(bad_row(format!(
                "row at height {height} is duplicated or out of order"
            )));
        }
        previous = Some(height);
        out.push((height, parse_changes(&row.bk_set_update_hex)?));
    }
    Ok(out)
}

/// Build the set at `target` from `snapshot` and `rows`, checking every step
/// against the block-id-tree leaves.
pub fn replay_to(
    snapshot: &BkSetSnapshot,
    rows: &[BkSetUpdateRow],
    leaves: &BTreeMap<u64, BlockLeaves>,
    target: &BkTarget,
) -> Result<ReplayedSet, ReplayError> {
    let parsed = checked_rows(snapshot, rows, target.height)?;
    let mut set = canonical_snapshot(&snapshot.signers)?;

    if target.height >= snapshot.anchor_height {
        for (height, changes) in &parsed {
            let l = leaves_of(leaves, *height)?;
            expect(*height, "l2-before-apply", l[LEAF_L2], &set)?;
            apply_block_changes(&mut set, changes)?;
            expect(*height, "l3-after-apply", l[LEAF_L3], &set)?;
        }
    } else {
        for (height, changes) in parsed.iter().rev() {
            let l = leaves_of(leaves, *height)?;
            expect(*height, "l3-before-undo", l[LEAF_L3], &set)?;
            undo_at(*height, &mut set, changes)?;
            expect(*height, "l2-after-undo", l[LEAF_L2], &set)?;
        }
    }

    let final_leaves = leaves_of(leaves, target.check.height())?;
    let expected = final_leaves[target.check.leaf_index()];
    expect(target.height, "final", expected, &set)?;
    Ok(ReplayedSet {
        pubkeys: set,
        commitment: expected,
        height: target.height,
    })
}

#[cfg(test)]
mod tests {
    use halo2_base::halo2_proofs::halo2curves::{
        bls12_381::{Fr as BlsScalar, G1Affine, G1},
        group::Curve,
    };

    use super::*;

    fn point(seed: u64) -> G1Affine {
        (G1::generator() * BlsScalar::from(seed)).to_affine()
    }

    /// Compressed key, as the sets hold them.
    fn key(seed: u64) -> Vec<u8> {
        point(seed).to_compressed_be().to_vec()
    }

    fn set(entries: &[(u16, u64)]) -> HashMap<u16, Vec<u8>> {
        entries.iter().map(|(i, s)| (*i, key(*s))).collect()
    }

    fn c(entries: &[(u16, u64)]) -> [u8; 32] {
        commitment(&set(entries))
    }

    /// A blob in the node's wire format with 96-byte uncompressed keys.
    fn blob(changes: &[(u32, u16, u64)]) -> String {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(changes.len() as u64).to_le_bytes());
        for (variant, index, seed) in changes {
            buf.extend_from_slice(&variant.to_le_bytes());
            buf.extend_from_slice(&index.to_le_bytes());
            buf.extend_from_slice(&96u64.to_le_bytes());
            buf.extend_from_slice(&point(*seed).to_uncompressed_be());
        }
        hex::encode(buf)
    }

    fn row(height: u64, changes: &[(u32, u16, u64)]) -> BkSetUpdateRow {
        BkSetUpdateRow {
            height: Some(height),
            bk_set_update_hex: blob(changes),
        }
    }

    fn leaves(l2: [u8; 32], l3: [u8; 32]) -> BlockLeaves {
        let mut l = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
        l[LEAF_L2] = l2;
        l[LEAF_L3] = l3;
        l
    }

    fn snap(anchor_height: u64, entries: &[(u16, u64)]) -> BkSetSnapshot {
        BkSetSnapshot {
            anchor_height,
            signers: set(entries),
        }
    }

    fn target(height: u64, check: FinalCheck) -> BkTarget {
        BkTarget {
            height,
            check,
        }
    }

    const ADD: u32 = 0;
    const REMOVE: u32 = 1;
    const OTHER: u32 = 2;

    // Sets used by the multi-row scenarios: S0 --17000--> S1 --18000--> S2.
    const S0: &[(u16, u64)] = &[(0, 1), (1, 2)];
    const S1: &[(u16, u64)] = &[(0, 1), (1, 2), (2, 3)];
    const S2: &[(u16, u64)] = &[(0, 1), (2, 3)];
    fn row_17000() -> BkSetUpdateRow {
        row(17000, &[(ADD, 2, 3)])
    }
    fn row_18000() -> BkSetUpdateRow {
        row(18000, &[(REMOVE, 1, 2)])
    }

    #[test]
    fn forward_applies_removes_before_adds() {
        let rows = [row(100, &[(ADD, 1, 9), (REMOVE, 1, 1)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(&[(0, 5), (1, 1)]), c(&[(0, 5), (1, 9)])));
        let out = replay_to(
            &snap(100, &[(0, 5), (1, 1)]),
            &rows,
            &lv,
            &target(100, FinalCheck::L3Of(100)),
        )
        .unwrap();
        assert_eq!(out.pubkeys, set(&[(0, 5), (1, 9)]));
        assert_eq!(out.height, 100);
        assert_eq!(out.commitment, c(&[(0, 5), (1, 9)]));
    }

    #[test]
    fn backward_undoes_in_mirrored_order() {
        let rows = [row(100, &[(ADD, 1, 9), (REMOVE, 1, 1)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(&[(0, 5), (1, 1)]), c(&[(0, 5), (1, 9)])));
        let out = replay_to(
            &snap(200, &[(0, 5), (1, 9)]),
            &rows,
            &lv,
            &target(99, FinalCheck::L2Of(100)),
        )
        .unwrap();
        assert_eq!(out.pubkeys, set(&[(0, 5), (1, 1)]));
    }

    #[test]
    fn forward_and_backward_meet_at_the_same_set() {
        let rows = [row_17000(), row_18000()];
        let mut lv = BTreeMap::new();
        lv.insert(16384, leaves(c(S0), c(S0)));
        lv.insert(17000, leaves(c(S0), c(S1)));
        lv.insert(18000, leaves(c(S1), c(S2)));
        let back = replay_to(
            &snap(19000, S2),
            &rows,
            &lv,
            &target(16384, FinalCheck::L3Of(16384)),
        )
        .unwrap();
        assert_eq!(back.pubkeys, set(S0));
        let forward = replay_to(
            &snap(16384, S0),
            &rows,
            &lv,
            &target(18000, FinalCheck::L3Of(18000)),
        )
        .unwrap();
        assert_eq!(forward.pubkeys, set(S2));
        // Walking forward from what the backward replay found leads back to the
        // snapshot.
        assert_eq!(forward.pubkeys, snap(19000, S2).signers);
    }

    #[test]
    fn target_equal_anchor_applies_row_at_anchor() {
        let rows = [row(100, &[(ADD, 2, 3)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S1)));
        let out = replay_to(
            &snap(100, S0),
            &rows,
            &lv,
            &target(100, FinalCheck::L3Of(100)),
        )
        .unwrap();
        assert_eq!(out.pubkeys, set(S1));
    }

    #[test]
    fn target_below_anchor_by_one() {
        assert_eq!(rows_to_fetch(100, 99), None);
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S0)));
        let out = replay_to(&snap(100, S0), &[], &lv, &target(99, FinalCheck::L2Of(100))).unwrap();
        assert_eq!(out.pubkeys, set(S0));
        assert_eq!(out.height, 99);
    }

    #[test]
    fn rows_to_fetch_ranges() {
        assert_eq!(rows_to_fetch(100, 150), Some((100, 150)));
        assert_eq!(rows_to_fetch(100, 100), Some((100, 100)));
        assert_eq!(rows_to_fetch(100, 98), Some((99, 99)));
        assert_eq!(rows_to_fetch(100, 50), Some((51, 99)));
        assert_eq!(rows_to_fetch(100, 99), None);
    }

    #[test]
    fn row_at_target_is_kept_backwards() {
        // The row at 150 is undone; the change at the target 100 stays.
        let rows = [row(150, &[(ADD, 2, 3)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(&[(0, 7)]), c(S0)));
        lv.insert(150, leaves(c(S0), c(S1)));
        let out = replay_to(
            &snap(200, S1),
            &rows,
            &lv,
            &target(100, FinalCheck::L3Of(100)),
        )
        .unwrap();
        assert_eq!(out.pubkeys, set(S0));
        // A row at the target itself lies outside (T, A).
        let rows = [row(100, &[(ADD, 2, 3)]), row(150, &[(ADD, 2, 3)])];
        let err = replay_to(
            &snap(200, S1),
            &rows,
            &lv,
            &target(100, FinalCheck::L3Of(100)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::BadRow { .. }));
    }

    #[test]
    fn genesis_target_checks_l2_of_block_one() {
        let mut lv = BTreeMap::new();
        lv.insert(1, leaves(c(S0), c(S0)));
        let out = replay_to(&snap(5, S0), &[], &lv, &target(0, FinalCheck::L2Of(1))).unwrap();
        assert_eq!(out.height, 0);
        assert_eq!(out.pubkeys, set(S0));
    }

    #[test]
    fn final_check_l2_of_next_does_not_need_leaves_of_target() {
        let rows = [row_17000()];
        let snapshot = snap(20000, S1);
        let mut lv = BTreeMap::new();
        lv.insert(16384, leaves(c(S0), c(S0)));
        lv.insert(17000, leaves(c(S0), c(S1)));
        let t = BkTarget {
            height: 16383,
            check: FinalCheck::L2Of(16384),
        };
        assert!(replay_to(&snapshot, &rows, &lv, &t).is_ok());
        assert!(!leaves_to_fetch(&rows, &t).contains(&16383));
        assert_eq!(leaves_to_fetch(&rows, &t), vec![16384, 17000]);
    }

    #[test]
    fn missing_leaves_are_reported_with_their_height() {
        let rows = [row_17000()];
        let lv = BTreeMap::new();
        let err = replay_to(
            &snap(17000, S0),
            &rows,
            &lv,
            &target(17000, FinalCheck::L3Of(17000)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::MissingLeaves {
            height: 17000
        }));
    }

    #[test]
    fn future_only_row_keeps_the_set() {
        let rows = [row(100, &[(OTHER, 0, 1)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S0)));
        let out = replay_to(
            &snap(100, S0),
            &rows,
            &lv,
            &target(100, FinalCheck::L3Of(100)),
        )
        .unwrap();
        assert_eq!(out.pubkeys, set(S0));
    }

    #[test]
    fn missing_row_forward_is_a_mismatch_at_the_next_row() {
        // The row at 17000 is missing.
        let rows = [row_18000()];
        let mut lv = BTreeMap::new();
        lv.insert(18000, leaves(c(S1), c(S2)));
        lv.insert(19000, leaves(c(S2), c(S2)));
        let err = replay_to(
            &snap(10000, S0),
            &rows,
            &lv,
            &target(19000, FinalCheck::L3Of(19000)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            height: 18000,
            step: "l2-before-apply",
            ..
        }));
    }

    #[test]
    fn missing_row_backward_is_a_mismatch_at_the_final_check() {
        // The row at 17000 is missing.
        let rows = [row_18000()];
        let mut lv = BTreeMap::new();
        lv.insert(16384, leaves(c(S0), c(S0)));
        lv.insert(18000, leaves(c(S1), c(S2)));
        let err = replay_to(
            &snap(19000, S2),
            &rows,
            &lv,
            &target(16384, FinalCheck::L3Of(16384)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            height: 16384,
            step: "final",
            ..
        }));
    }

    #[test]
    fn swapped_rows_are_bad_rows() {
        let rows = [row_18000(), row_17000()];
        let t = target(19000, FinalCheck::L3Of(19000));
        let err = replay_to(&snap(10000, S0), &rows, &BTreeMap::new(), &t).unwrap_err();
        assert!(matches!(err, ReplayError::BadRow { .. }));
    }

    #[test]
    fn wrong_l2_and_wrong_l3_are_mismatches() {
        let rows = [row(100, &[(ADD, 2, 3)])];
        let bad = [9u8; 32];
        let t = target(100, FinalCheck::L3Of(100));

        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(bad, c(S1)));
        let err = replay_to(&snap(100, S0), &rows, &lv, &t).unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            step: "l2-before-apply",
            ..
        }));

        lv.insert(100, leaves(c(S0), bad));
        let err = replay_to(&snap(100, S0), &rows, &lv, &t).unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            step: "l3-after-apply",
            ..
        }));

        let t = target(99, FinalCheck::L2Of(100));
        lv.insert(100, leaves(c(S0), bad));
        let err = replay_to(&snap(101, S1), &rows, &lv, &t).unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            step: "l3-before-undo",
            ..
        }));

        lv.insert(100, leaves(bad, c(S1)));
        let err = replay_to(&snap(101, S1), &rows, &lv, &t).unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            step: "l2-after-undo",
            ..
        }));
    }

    #[test]
    fn result_not_equal_final_leaf_is_a_mismatch() {
        let rows = [row(100, &[(ADD, 2, 3)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S1)));
        lv.insert(101, leaves(c(S0), c(S0)));
        let err = replay_to(
            &snap(100, S0),
            &rows,
            &lv,
            &target(100, FinalCheck::L2Of(101)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            height: 100,
            step: "final",
            ..
        }));
    }

    #[test]
    fn undo_added_with_other_key_fails() {
        // The set holds key 2 at index 1 while the row says key 9 was added there.
        let rows = [row(100, &[(ADD, 1, 9)])];
        let held = [(1u16, 2u64)];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(&[(0, 5)]), c(&held)));
        let err = replay_to(
            &snap(101, &held),
            &rows,
            &lv,
            &target(99, FinalCheck::L2Of(100)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Undo {
            height: 100,
            signer_index: 1,
            reason: "added key differs"
        }));
    }

    #[test]
    fn undo_removed_into_occupied_index_fails() {
        let rows = [row(100, &[(REMOVE, 1, 1)])];
        let held = [(1u16, 2u64)];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(&[(1, 1)]), c(&held)));
        let err = replay_to(
            &snap(101, &held),
            &rows,
            &lv,
            &target(99, FinalCheck::L2Of(100)),
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Undo {
            height: 100,
            signer_index: 1,
            reason: "index occupied"
        }));
        assert!(err.to_string().contains("100"));
    }

    #[test]
    fn null_height_row_is_bad_row() {
        let rows = [BkSetUpdateRow {
            height: None,
            bk_set_update_hex: blob(&[(ADD, 2, 3)]),
        }];
        let t = target(100, FinalCheck::L3Of(100));
        let err = replay_to(&snap(100, S0), &rows, &BTreeMap::new(), &t).unwrap_err();
        assert!(matches!(err, ReplayError::BadRow { .. }));
    }

    #[test]
    fn duplicated_row_is_bad_row() {
        let rows = [row(100, &[(ADD, 2, 3)]), row(100, &[(ADD, 2, 3)])];
        let t = target(200, FinalCheck::L3Of(200));
        let err = replay_to(&snap(100, S0), &rows, &BTreeMap::new(), &t).unwrap_err();
        assert!(matches!(err, ReplayError::BadRow { .. }));
    }

    #[test]
    fn out_of_range_and_empty_rows_are_bad_rows() {
        let t = target(200, FinalCheck::L3Of(200));
        let rows = [row(99, &[(ADD, 2, 3)])];
        assert!(matches!(
            replay_to(&snap(100, S0), &rows, &BTreeMap::new(), &t),
            Err(ReplayError::BadRow { .. })
        ));
        let rows = [BkSetUpdateRow {
            height: Some(150),
            bk_set_update_hex: String::new(),
        }];
        assert!(matches!(
            replay_to(&snap(100, S0), &rows, &BTreeMap::new(), &t),
            Err(ReplayError::BadRow { .. })
        ));
        assert!(matches!(
            parse_changes("zz"),
            Err(ReplayError::BadRow { .. })
        ));
    }

    #[test]
    fn mismatch_display_names_height_and_step() {
        let e = ReplayError::Mismatch {
            height: 18000,
            step: "final",
            expected: [0; 32],
            actual: [1; 32],
        };
        let text = e.to_string();
        assert!(text.contains("18000") && text.contains("final"));
    }

    #[test]
    fn off_curve_snapshot_key_is_bad_row_not_a_panic() {
        let mut s = snap(100, S0);
        s.signers.insert(1, vec![0xff; 48]);
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S0)));
        let err = replay_to(&s, &[], &lv, &target(100, FinalCheck::L3Of(100))).unwrap_err();
        assert!(
            matches!(&err, ReplayError::BadRow { reason } if reason.contains("snapshot signer 1"))
        );
    }

    #[test]
    fn wrong_length_snapshot_key_is_bad_row() {
        let mut s = snap(100, S0);
        s.signers.insert(1, vec![1; 47]);
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S0)));
        let err = replay_to(&s, &[], &lv, &target(100, FinalCheck::L3Of(100))).unwrap_err();
        assert!(matches!(err, ReplayError::BadRow { .. }));
    }

    #[test]
    fn uncompressed_snapshot_replays_like_compressed() {
        let rows = [row(100, &[(ADD, 2, 3)])];
        let mut lv = BTreeMap::new();
        lv.insert(100, leaves(c(S0), c(S1)));
        let t = target(100, FinalCheck::L3Of(100));
        let compressed_run = replay_to(&snap(100, S0), &rows, &lv, &t).unwrap();
        let mut s = snap(100, S0);
        for (seed, i) in [(1u64, 0u16), (2, 1)] {
            s.signers
                .insert(i, point(seed).to_uncompressed_be().to_vec());
        }
        let uncompressed_run = replay_to(&s, &rows, &lv, &t).unwrap();
        assert_eq!(uncompressed_run, compressed_run);
        // Undo compares keys too: an uncompressed snapshot must undo a blob row.
        let t = target(99, FinalCheck::L2Of(100));
        let mut s = snap(101, S1);
        for (seed, i) in [(1u64, 0u16), (2, 1), (3, 2)] {
            s.signers
                .insert(i, point(seed).to_uncompressed_be().to_vec());
        }
        let back = replay_to(&s, &rows, &lv, &t).unwrap();
        assert_eq!(back.pubkeys, set(S0));
    }

    #[test]
    fn transient_classification() {
        assert!(ReplayError::NoSnapshot.is_transient());
        assert!(ReplayError::MissingLeaves {
            height: 1
        }
        .is_transient());
        assert!(ReplayError::Query(anyhow::anyhow!("x")).is_transient());
        assert!(!ReplayError::BadRow {
            reason: String::new()
        }
        .is_transient());
        assert!(!ReplayError::ServerTooOld.is_transient());
    }
}
