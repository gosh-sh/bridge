//! Fetch and reconstruct the Block Keeper (BK) set from a node's GraphQL API.

use std::collections::HashMap;

use anyhow::{bail, Context};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use tracing::debug;

use crate::gql_client::{BkSetUpdateWithAttestations, GqlClient, DEFAULT_THREAD_ID_HEX};

/// Page size for `next_update_after` cursor walk. Near-head callers (prover
/// caught up to within one page's worth of rotation events) resolve in a
/// single round trip via the ascending-order early-return.
const NEXT_UPDATE_PAGE_SIZE: u32 = 500;

/// Cursor-walk over `bkSetUpdates`: returns the *next* rotation event whose
/// chain height is strictly greater than `cursor_seq_no`, regardless of W·P
/// alignment. Returns `Ok(None)` when the chain has no rotation past the
/// cursor yet (i.e. the prover is caught up on rotations).
///
/// This is the missing-detector that the Phase-1 L2≠L3-at-thinned-key-block
/// check could not provide — real rotations land on arbitrary heights, almost
/// none of them W·P-aligned.
///
/// # Correctness
///
/// AN's `bkSetUpdates` Relay connection returns edges in ascending
/// `chain_order`, which coincides with ascending block height on any
/// well-formed chain. We paginate forward via `after: cursor` and return the
/// min-height past-`cursor_seq_no` entry from the *first* page that
/// contains any such entry — ascending order guarantees no earlier page has
/// one and no later page can have a smaller past-cursor height.
///
/// An [`assert_ascending_by_height`] check runs on every page against a
/// running high-water mark. If a page ever returns an out-of-order height,
/// the function errors out rather than silently returning a wrong answer —
/// this guards the early-return against a future schema / pagination-order
/// change.
///
/// # Cost
///
/// Round trips = `ceil(events_past_cursor_up_to_first_hit / NEXT_UPDATE_PAGE_SIZE)`.
/// On a chain where the prover is caught up to head this is O(1). On a
/// long-lagging prover it degrades linearly but is bounded by the
/// hard 10_000-page ceiling.
///
/// The returned struct includes `block_id`, `height`, and the raw
/// `bk_set_update_hex` blob; the caller parses the latter via
/// [`parse_bk_set_changes_pub`] to derive the new pubkey table.
pub async fn next_update_after(
    client: &GqlClient,
    cursor_seq_no: u64,
) -> anyhow::Result<Option<BkSetUpdateWithAttestations>> {
    let mut after: Option<String> = None;
    let mut pages = 0usize;
    let mut last_height_seen: u64 = 0;
    loop {
        let (page, next) = client
            .query_bk_set_updates_range(
                DEFAULT_THREAD_ID_HEX,
                Some(cursor_seq_no.saturating_add(1)),
                None,
                NEXT_UPDATE_PAGE_SIZE,
                after.as_deref(),
            )
            .await
            .with_context(|| format!("next_update_after page {}", pages))?;
        pages += 1;
        debug!(
            "next_update_after page {}: {} events (cursor advance -> {:?})",
            pages,
            page.len(),
            next,
        );

        assert_ascending_by_height(&page, &mut last_height_seen)
            .with_context(|| format!("next_update_after page {}: order sanity", pages))?;

        if let Some(hit) = pick_next_past_cursor(&page, cursor_seq_no) {
            return Ok(Some(hit.clone()));
        }

        match next {
            Some(c) => after = Some(c),
            None => return Ok(None),
        }
        if pages > 10_000 {
            bail!(
                "next_update_after: refusing to paginate past 10_000 pages \
                 (likely a server-side pagination bug) at cursor {:?}",
                after,
            );
        }
    }
}

/// Highest `bkSetUpdates.height ≤ cap_seq_no` observed in the chain's GQL
/// retention window, or `0` when the window contains no such event.
///
/// Paired with the Solidity constructor arg `genesisLastBkSetUpdateSeqNo`:
/// a fresh bridge seeded at `cap_seq_no` records this cursor on-chain so
/// the daemon's bk-update drain only ever considers rotations past the
/// seed. The historical rotations ≤ cap are already folded into the live
/// BK-set commitment the operator provides — re-applying them would move
/// the prover *away* from the current chain state.
///
/// # Correctness
///
/// `query_bk_set_updates_paged(height_end = cap_seq_no, …)` returns edges
/// in ascending `chain_order` (and therefore ascending height — see
/// [`next_update_after`] above). Walking every page to exhaustion and
/// taking the final page's largest-height entry is the maximum. Returns
/// `0` when the first page is empty, which the Solidity constructor
/// (default `vm.envOr(…, 0)`) and `BridgeState::new()` already treat as
/// "no rotation applied yet".
///
/// # Cost
///
/// Linear in the number of events ≤ `cap_seq_no`. The 10_000-page ceiling
/// of `next_update_after` is mirrored here as a defensive guard.
pub async fn highest_bk_set_update_le(client: &GqlClient, cap_seq_no: u64) -> anyhow::Result<u64> {
    Ok(client
        .query_last_bk_set_update_at_or_below(DEFAULT_THREAD_ID_HEX, cap_seq_no)
        .await
        .context("highest_bk_set_update_le")?
        .unwrap_or(0))
}

/// Pick the smallest-height entry with `height > cursor_seq_no` from a
/// single page. Extracted so `next_update_after`'s per-page decision is
/// unit-testable without a live GQL server.
///
/// Returns `None` when no entry qualifies — the caller should fetch the
/// next page and re-apply.
pub(crate) fn pick_next_past_cursor(
    page: &[BkSetUpdateWithAttestations],
    cursor_seq_no: u64,
) -> Option<&BkSetUpdateWithAttestations> {
    page.iter()
        .filter(|u| u.height.map(|h| h > cursor_seq_no).unwrap_or(false))
        .min_by_key(|u| u.height.unwrap_or(u64::MAX))
}

/// Assert that heights in `page` are non-decreasing relative to
/// `last_height_seen`. Updates `last_height_seen` in place to the max
/// height observed on success. Entries with `height = None` are skipped
/// (some early DB rows lack a height column; they cannot break the
/// invariant on their own).
///
/// Extracted so the ascending-order invariant `next_update_after` relies on
/// is unit-testable and the error text is exercised by CI.
pub(crate) fn assert_ascending_by_height(
    page: &[BkSetUpdateWithAttestations],
    last_height_seen: &mut u64,
) -> anyhow::Result<()> {
    for u in page {
        if let Some(h) = u.height {
            if h < *last_height_seen {
                bail!(
                    "bkSetUpdates page returned non-ascending height: \
                     saw {} after {} — next_update_after early-return \
                     assumption violated (schema drift?)",
                    h,
                    *last_height_seen,
                );
            }
            *last_height_seen = h;
        }
    }
    Ok(())
}

/// Public wrapper around [`parse_bk_set_changes`] so the prover daemon can
/// derive the post-update pubkey table from the raw blob returned by
/// [`next_update_after`]. Returns `(variant, signer_idx, pubkey_bytes)`
/// triples; variant=0 means Added, variant=1 means Removed.
pub fn parse_bk_set_changes_pub(blob: &[u8]) -> Vec<(u32, u16, Vec<u8>)> {
    parse_bk_set_changes(blob)
}

/// Variant constants re-exported so the prover daemon doesn't have to repeat
/// the discriminant table.
pub const BK_CHANGE_VARIANT_ADDED: u32 = BK_CHANGE_ADDED;
pub const BK_CHANGE_VARIANT_REMOVED: u32 = BK_CHANGE_REMOVED;

/// Read a BK set from a JSON fixture file, for tests and offline tools.
///
/// The daemons do not read BK-set files: they build the set from GraphQL.
///
/// # File schema
///
/// A flat JSON object mapping **decimal signer index** (as a string) to a
/// **hex-encoded BLS12-381 G1 pubkey**:
///
/// ```json
/// {
///   "0": "8f1a...c2",
///   "1": "b73e...41",
///   "2": "a509...ff"
/// }
/// ```
///
/// * Keys parse as `u16` — the string wrapper is just JSON's map-key
///   constraint, values are the raw signer indices used everywhere else
///   in the pipeline.
/// * Values may be **48-byte compressed** (canonical) or **96-byte
///   uncompressed**; both encodings are accepted and [`normalize_bk_set_pubkeys`]
///   collapses uncompressed keys to the compressed form before returning.
///   Any other length errors.
pub fn load_bk_set_fixture_json(path: &str) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    let data = std::fs::read_to_string(path).context("failed to read BK set config")?;
    let map: HashMap<String, String> = serde_json::from_str(&data)?;
    let mut bk_set = HashMap::new();
    for (k, v) in &map {
        let idx: u16 = k.parse().context("invalid signer index in config")?;
        let pk_bytes = hex::decode(v).context("invalid pubkey hex in config")?;
        if pk_bytes.len() != 48 && pk_bytes.len() != 96 {
            bail!(
                "pubkey for signer {} has {} bytes, expected 48 or 96",
                idx,
                pk_bytes.len()
            );
        }
        bk_set.insert(idx, pk_bytes);
    }
    normalize_bk_set_pubkeys(bk_set)
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

/// Normalize pubkeys: compress 96-byte uncompressed keys to 48-byte compressed.
///
/// Public because the prover daemon's bk-update drain mixes 96-byte delta
/// pubkeys (from `parse_bk_set_changes_pub`) into a 48-byte base set and
/// needs to normalize the resulting map before feeding it to BLS-aware
/// helpers like `compute_bk_set_poseidon`.
pub fn normalize_bk_set_pubkeys(
    bk_set: HashMap<u16, Vec<u8>>,
) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    let mut normalized = HashMap::new();
    for (idx, pk_bytes) in bk_set {
        let compressed = match pk_bytes.len() {
            48 => pk_bytes,
            96 => {
                let bytes: [u8; 96] = pk_bytes
                    .clone()
                    .try_into()
                    .map_err(|_| anyhow::format_err!("invalid 96-byte pubkey"))?;
                let opt_be = G1Affine::from_uncompressed_be(&bytes);
                let pt = if bool::from(opt_be.is_some()) {
                    opt_be.unwrap()
                } else {
                    let opt_le = G1Affine::from_uncompressed_le(&bytes);
                    if bool::from(opt_le.is_some()) {
                        opt_le.unwrap()
                    } else {
                        bail!("failed to deserialize 96-byte pubkey for signer {}", idx);
                    }
                };
                pt.to_compressed_be().to_vec()
            }
            other => bail!(
                "unexpected pubkey size {} for signer {} (expected 48 or 96)",
                other,
                idx
            ),
        };
        normalized.insert(idx, compressed);
    }
    Ok(normalized)
}

/// Variant discriminants for BlockKeeperSetChange enum (bincode u32).
const BK_CHANGE_ADDED: u32 = 0;
const BK_CHANGE_REMOVED: u32 = 1;

/// Parse a bk_set_update blob by scanning for pubkey markers.
///
/// Scans for the `u64(96)` pubkey length marker preceded by variant + signer_index.
fn parse_bk_set_changes(blob: &[u8]) -> Vec<(u32, u16, Vec<u8>)> {
    let mut results = Vec::new();
    if blob.len() < 16 {
        return results;
    }
    let mut i = 8; // skip num_changes u64 header
    while i + 14 + 96 <= blob.len() {
        let variant = u32::from_le_bytes(blob[i..i + 4].try_into().unwrap());
        let signer_idx = u16::from_le_bytes(blob[i + 4..i + 6].try_into().unwrap());
        let pk_len = u64::from_le_bytes(blob[i + 6..i + 14].try_into().unwrap());

        if variant <= 3 && pk_len == 96 && (signer_idx as u32) < 100_000 {
            let pk = blob[i + 14..i + 14 + 96].to_vec();
            results.push((variant, signer_idx, pk));
            i += 14 + 96;
            continue;
        }
        i += 1;
    }
    results
}

#[cfg(test)]
mod pagination_tests {
    //! Unit tests for the pure per-page helpers behind `next_update_after`.
    //! No network — these lock in the algorithm's decision logic and the
    //! ascending-order sanity check independently of GQL wire behavior.
    use super::*;

    fn mk_event(height: Option<u64>, id: &str) -> BkSetUpdateWithAttestations {
        BkSetUpdateWithAttestations {
            block_id: id.into(),
            bk_set_update_hex: String::new(),
            height,
            attestations: Vec::new(),
            chain_order: None,
        }
    }

    #[test]
    fn pick_returns_min_height_past_cursor() {
        // Page intentionally shuffled to prove `min_by_key` (not first-hit)
        // is what selects — we do not depend on caller-side ordering when
        // choosing within a page.
        let page = vec![
            mk_event(Some(100), "a"),
            mk_event(Some(50), "b"),
            mk_event(Some(75), "c"),
            mk_event(Some(200), "d"),
        ];
        let hit = pick_next_past_cursor(&page, 60).expect("60 < 75 < 100 < 200");
        assert_eq!(hit.height, Some(75));
        assert_eq!(hit.block_id, "c");
    }

    #[test]
    fn pick_returns_none_when_all_at_or_before_cursor() {
        let page = vec![
            mk_event(Some(100), "a"),
            mk_event(Some(50), "b"),
            mk_event(Some(100), "c"), // exactly at cursor — strictly-greater rejects
        ];
        assert!(pick_next_past_cursor(&page, 100).is_none());
    }

    #[test]
    fn pick_ignores_events_with_missing_height() {
        // `height: None` rows exist in early DB rows; they must never win
        // over a real past-cursor candidate.
        let page = vec![
            mk_event(None, "a"),
            mk_event(Some(150), "b"),
            mk_event(None, "c"),
        ];
        let hit = pick_next_past_cursor(&page, 100).unwrap();
        assert_eq!(hit.block_id, "b");
    }

    #[test]
    fn ascending_within_page_ok() {
        let page = vec![
            mk_event(Some(10), "a"),
            mk_event(Some(20), "b"),
            mk_event(Some(30), "c"),
        ];
        let mut last = 0;
        assert_ascending_by_height(&page, &mut last).unwrap();
        assert_eq!(last, 30, "high-water mark must advance to page max");
    }

    #[test]
    fn ascending_across_pages_ok() {
        let p1 = vec![mk_event(Some(10), "a"), mk_event(Some(20), "b")];
        let p2 = vec![mk_event(Some(25), "c"), mk_event(Some(40), "d")];
        let mut last = 0;
        assert_ascending_by_height(&p1, &mut last).unwrap();
        assert_ascending_by_height(&p2, &mut last).unwrap();
        assert_eq!(last, 40);
    }

    #[test]
    fn ascending_rejects_descending_within_page() {
        let page = vec![
            mk_event(Some(30), "a"),
            mk_event(Some(20), "b"), // regression relative to prior in same page
        ];
        let mut last = 0;
        let err = assert_ascending_by_height(&page, &mut last).unwrap_err();
        assert!(
            err.to_string().contains("non-ascending"),
            "error must name the invariant so schema-drift is diagnosable: {err}"
        );
    }

    #[test]
    fn ascending_rejects_descending_across_pages() {
        // Guards the cross-page invariant — a subsequent page must never
        // start below the prior page's max.
        let p1 = vec![mk_event(Some(30), "a")];
        let p2 = vec![mk_event(Some(20), "b")];
        let mut last = 0;
        assert_ascending_by_height(&p1, &mut last).unwrap();
        let err = assert_ascending_by_height(&p2, &mut last).unwrap_err();
        assert!(err.to_string().contains("non-ascending"));
    }

    #[test]
    fn ascending_ignores_missing_heights() {
        // Missing-height rows are transparent — they neither advance the
        // watermark nor can they violate ordering.
        let page = vec![
            mk_event(None, "a"),
            mk_event(Some(50), "b"),
            mk_event(None, "c"),
            mk_event(Some(100), "d"),
        ];
        let mut last = 0;
        assert_ascending_by_height(&page, &mut last).unwrap();
        assert_eq!(last, 100);
    }

    #[test]
    fn ascending_equal_heights_ok() {
        // Same-height events can legitimately appear on the same page
        // (same block emits multiple rotation deltas); `<` — not `<=` —
        // must be the rejection criterion.
        let page = vec![
            mk_event(Some(500), "a"),
            mk_event(Some(500), "b"),
        ];
        let mut last = 0;
        assert_ascending_by_height(&page, &mut last).unwrap();
        assert_eq!(last, 500);
    }
}

#[cfg(test)]
mod live_tests {
    //! Live-network tests — ignored by default. Run with:
    //!   cargo test -p bridge-prover-lib --release -- --ignored next_update_after
    use super::*;

    const SHELLNET: &str = "https://shellnet.ackinacki.org/graphql";

    /// Reference data captured 2026-06-15 from shellnet's last 20 bkSetUpdates:
    ///   [2584711, 2584894, 2585790, 2585952, 2586276,
    ///    2595553, 2595737, 2596659, 2596821, 2597146,
    ///    2606393, 2606577, 2607530, 2607689, 2608018,
    ///    2617235, 2617418, 2618398, 2618558, 2618888]
    /// These are stable historical events — assertions below pin to them.
    #[tokio::test]
    #[ignore]
    async fn next_update_after_finds_known_rotation() {
        let client = crate::gql_client::create_client(SHELLNET).unwrap();
        // Cursor strictly before 2584711 should return that exact event.
        let r = next_update_after(&client, 2_584_710).await.unwrap();
        let u = r.expect("at least one rotation past 2584710 must exist");
        assert_eq!(
            u.height,
            Some(2_584_711),
            "expected next rotation past 2584710 to be at height 2584711, got {:?}",
            u.height
        );
        assert!(!u.bk_set_update_hex.is_empty(), "blob must be non-empty");
        let changes = parse_bk_set_changes_pub(&hex::decode(&u.bk_set_update_hex).unwrap());
        assert!(!changes.is_empty(), "must parse at least one change");
    }

    #[tokio::test]
    #[ignore]
    async fn next_update_after_handles_cursor_inside_burst() {
        let client = crate::gql_client::create_client(SHELLNET).unwrap();
        // Cursor exactly at 2584711 should skip past it and return 2584894 (next in burst 1).
        let r = next_update_after(&client, 2_584_711).await.unwrap();
        let u = r.expect("next event after 2584711 must exist");
        assert_eq!(u.height, Some(2_584_894));
    }

    /// Regression against the removed `NEXT_UPDATE_AFTER_LOOKBACK = 100`
    /// silent-skip bug: cursor `= 0` walks paginated `bkSetUpdates` from the
    /// very first event, verifying that even on a chain with more rotations
    /// than one page fits (or on any prover restart after a long lag) the
    /// earliest-past-cursor event is returned rather than one from the
    /// most-recent page. The shipped fixture's first-ever shellnet rotation
    /// at `2_584_711` pins the answer.
    #[tokio::test]
    #[ignore]
    async fn next_update_after_paginates_from_genesis() {
        let client = crate::gql_client::create_client(SHELLNET).unwrap();
        let r = next_update_after(&client, 0).await.unwrap();
        let u = r.expect("chain must have at least one bkSetUpdate");
        assert_eq!(
            u.height,
            Some(2_584_711),
            "earliest-ever rotation on shellnet expected at height 2584711, got {:?}",
            u.height,
        );
    }
}
