//! Fetch and reconstruct the Block Keeper (BK) set from a node's GraphQL API.

use std::collections::HashMap;

use anyhow::{bail, Context};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use tracing::{debug, info};

use crate::gql_client::{
    BkSetUpdateWithAttestations, GqlClient, GRAPHQL_SIGNED_INT_MAX,
};

/// Page size for `next_update_after` cursor walk. Sized to match
/// `BK_SET_AT_HEIGHT_PAGE_SIZE`: near-head callers (prover caught up to
/// within one page's worth of rotation events) resolve in a single round
/// trip via the ascending-order early-return.
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
            .query_bk_set_updates_paged(
                GRAPHQL_SIGNED_INT_MAX,
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

/// Pick the smallest-height entry with `height > cursor_seq_no` from a
/// single page. Extracted so `next_update_after`'s per-page decision is
/// unit-testable without a live GQL server.
///
/// Returns `None` when no entry qualifies — the caller should fetch the
/// next page and re-apply.
pub(crate) fn pick_next_past_cursor<'a>(
    page: &'a [BkSetUpdateWithAttestations],
    cursor_seq_no: u64,
) -> Option<&'a BkSetUpdateWithAttestations> {
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

/// Page size for `bk_set_at_height` cursor pagination. Sized to be gentle
/// on the node's DB while still finishing large scans in tens of round-trips.
const BK_SET_AT_HEIGHT_PAGE_SIZE: u32 = 500;

/// Reconstruct the BK set active at chain height `target_height` by folding
/// `bkSetUpdates` (height <= target_height, chronological) onto the caller-
/// supplied `genesis_bk_set`.
///
/// Why this exists: AN's `bkSetUpdates` is a delta log; it does *not* emit
/// the genesis committee as a synthetic `Added` event. The old `fetch_bk_set`
/// replayed the log from ∅ and so returned the wrong set on any rotating
/// chain. `bk_set_at_height` is the correct cold-start primitive — the
/// caller provides the genesis snapshot (from a JSON config), and this
/// function applies every rotation up to and including `target_height`.
///
/// Cost: O(rotations-since-genesis-up-to-N), paginated in
/// `BK_SET_AT_HEIGHT_PAGE_SIZE`-sized chunks over the node's GraphQL cursor.
/// On a healthy chain this is quick even for long lag; on a chain with
/// millions of rotations the caller should batch-verify progress via logs.
///
/// Preconditions:
/// - `genesis_bk_set` must be non-empty (typically loaded from a JSON config)
///   and contain 48- or 96-byte compressed/uncompressed BLS pubkeys; keys are
///   normalized to 48-byte compressed form before folding.
/// - `target_height >= 1` (returning genesis unchanged for target_height=0 is
///   allowed and short-circuits without a network call).
pub async fn bk_set_at_height(
    client: &GqlClient,
    genesis_bk_set: HashMap<u16, Vec<u8>>,
    target_height: u64,
) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    if genesis_bk_set.is_empty() {
        bail!("bk_set_at_height: genesis_bk_set is empty — cold start needs a non-empty anchor");
    }
    let base = normalize_bk_set_pubkeys(genesis_bk_set)?;
    if target_height == 0 {
        info!(
            "bk_set_at_height: target_height=0, returning genesis snapshot ({} signers)",
            base.len()
        );
        return Ok(base);
    }

    let mut cursor: Option<String> = None;
    let mut collected: Vec<BkSetUpdateWithAttestations> = Vec::new();
    let mut pages = 0usize;
    loop {
        let (mut page, next) = client
            .query_bk_set_updates_paged(
                target_height,
                BK_SET_AT_HEIGHT_PAGE_SIZE,
                cursor.as_deref(),
            )
            .await
            .with_context(|| {
                format!(
                    "query_bk_set_updates_paged failed at page {} (cursor={:?})",
                    pages, cursor
                )
            })?;
        pages += 1;
        debug!(
            "bk_set_at_height page {}: {} events (cursor advance -> {:?})",
            pages,
            page.len(),
            next
        );
        collected.append(&mut page);
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
        if pages > 10_000 {
            bail!(
                "bk_set_at_height: refusing to paginate past 10_000 pages \
                 (likely a server-side pagination bug) at cursor {:?}",
                cursor
            );
        }
    }

    info!(
        "bk_set_at_height: fetched {} rotation events across {} pages (target_height={})",
        collected.len(),
        pages,
        target_height
    );

    fold_bk_updates(base, &collected)
}

/// Pure fold: applies rotation events (in chronological order) to `base`.
///
/// Extracted from `bk_set_at_height` so unit tests can drive it with
/// synthetic events without a live GraphQL server. Events whose `height`
/// is `None` are applied as-is (the DB row lacked a height column) — this
/// matches AN's behavior on very early rows and keeps the fold total.
///
/// The returned map is normalized to 48-byte compressed pubkeys.
pub fn fold_bk_updates(
    mut base: HashMap<u16, Vec<u8>>,
    events: &[BkSetUpdateWithAttestations],
) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    // Sort by (height, chain_order) so callers who feed unordered pages
    // still get chronological application. `chain_order` is AN's canonical
    // tiebreaker for events at the same height.
    let mut ordered: Vec<&BkSetUpdateWithAttestations> = events.iter().collect();
    ordered.sort_by(|a, b| {
        let ha = a.height.unwrap_or(u64::MAX);
        let hb = b.height.unwrap_or(u64::MAX);
        ha.cmp(&hb).then_with(|| {
            a.chain_order
                .as_deref()
                .unwrap_or("")
                .cmp(b.chain_order.as_deref().unwrap_or(""))
        })
    });

    let mut adds = 0usize;
    let mut removes = 0usize;
    for u in ordered {
        if u.bk_set_update_hex.is_empty() {
            continue;
        }
        let blob = hex::decode(&u.bk_set_update_hex)
            .with_context(|| format!("decode bk_set_update at block {}", u.block_id))?;
        for (variant, signer_idx, pk) in parse_bk_set_changes(&blob) {
            match variant {
                BK_CHANGE_ADDED => {
                    base.insert(signer_idx, pk);
                    adds += 1;
                }
                BK_CHANGE_REMOVED => {
                    base.remove(&signer_idx);
                    removes += 1;
                }
                _ => {} // FutureAdd/VersionChange etc. — no effect on active set
            }
        }
    }

    info!(
        "fold_bk_updates: applied {} adds, {} removes; result has {} active signers",
        adds,
        removes,
        base.len()
    );

    if base.is_empty() {
        bail!(
            "fold_bk_updates: result is empty after {} adds / {} removes — \
             genesis anchor or update log is inconsistent",
            adds,
            removes
        );
    }
    normalize_bk_set_pubkeys(base)
}

/// Variant constants re-exported so the prover daemon doesn't have to repeat
/// the discriminant table.
pub const BK_CHANGE_VARIANT_ADDED: u32 = BK_CHANGE_ADDED;
pub const BK_CHANGE_VARIANT_REMOVED: u32 = BK_CHANGE_REMOVED;

/// Load BK set from a JSON config file (fallback).
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
///
/// This is the format both `bridge-prover-daemon` and `bridge-relayer-daemon`
/// consume via `bridge_prover_lib::bk_set_bootstrap::load_bk_set`; the
/// pointed-to file is named by the `BRIDGE_BK_SET_CONFIG` env var (or a
/// clap flag on the relayer side). It represents either the **current** BK
/// set (`mode=file`) or a **genesis anchor** to be folded forward
/// (`mode=fold_at_height`).
pub fn load_bk_set_from_config(path: &str) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
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
mod fold_tests {
    //! Unit tests for the pure `fold_bk_updates` fold — no network.
    use super::*;

    /// Build a synthetic bk_set_update blob in AN's on-wire format:
    /// `[num_changes u64 LE] [variant u32 LE, signer_idx u16 LE, pk_len u64 LE = 96, 96-byte pk]*`
    fn encode_changes(changes: &[(u32, u16, [u8; 96])]) -> String {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(changes.len() as u64).to_le_bytes());
        for (variant, idx, pk) in changes {
            buf.extend_from_slice(&variant.to_le_bytes());
            buf.extend_from_slice(&idx.to_le_bytes());
            buf.extend_from_slice(&96u64.to_le_bytes());
            buf.extend_from_slice(pk);
        }
        hex::encode(buf)
    }

    /// Produce a valid 96-byte uncompressed G1 encoding via generator×scalar.
    /// The scalar is derived from `seed` so different indices produce distinct
    /// pubkeys — matching how real BK signers differ. All results decompress
    /// cleanly under `normalize_bk_set_pubkeys`.
    fn uncompressed_pk(seed: u64) -> [u8; 96] {
        use halo2_base::halo2_proofs::halo2curves::bls12_381::{Fr as BlsScalar, G1Affine, G1};
        use halo2_base::halo2_proofs::halo2curves::group::Curve;
        let s = BlsScalar::from(seed.max(1));
        let p: G1Affine = (G1::generator() * s).to_affine();
        p.to_uncompressed_be()
    }

    fn synthetic_event(
        height: u64,
        chain_order: &str,
        changes: &[(u32, u16, [u8; 96])],
    ) -> BkSetUpdateWithAttestations {
        BkSetUpdateWithAttestations {
            block_id: format!("h{height}"),
            bk_set_update_hex: encode_changes(changes),
            height: Some(height),
            attestations: Vec::new(),
            chain_order: Some(chain_order.to_string()),
        }
    }

    #[test]
    fn fold_applies_add_then_remove_in_chronological_order() {
        // Genesis: signers 0 and 1.
        let pk0 = uncompressed_pk(1);
        let pk1 = uncompressed_pk(2);
        let pk2 = uncompressed_pk(3);
        let mut genesis = HashMap::new();
        genesis.insert(0u16, pk0.to_vec());
        genesis.insert(1u16, pk1.to_vec());

        // Two events: add signer 2 at height 100, remove signer 0 at height 200.
        let events = vec![
            synthetic_event(100, "a", &[(BK_CHANGE_ADDED, 2u16, pk2)]),
            synthetic_event(200, "b", &[(BK_CHANGE_REMOVED, 0u16, [0u8; 96])]),
        ];
        let result = fold_bk_updates(genesis, &events).expect("fold succeeds");
        // Expect signers {1, 2}. All pubkeys normalized to 48 bytes.
        assert_eq!(result.len(), 2);
        assert!(result.contains_key(&1));
        assert!(result.contains_key(&2));
        assert!(!result.contains_key(&0));
        for (idx, pk) in &result {
            assert_eq!(pk.len(), 48, "signer {} pk should be compressed", idx);
        }
    }

    #[test]
    fn fold_sorts_out_of_order_events_by_height() {
        // Feed events in REVERSE chronological order; fold must sort by height.
        // If it didn't, remove-then-add would leave the set with 2 (from add
        // at the earlier height being applied *after* the later remove).
        let pk0 = uncompressed_pk(1);
        let pk2 = uncompressed_pk(3);
        let mut genesis = HashMap::new();
        genesis.insert(0u16, pk0.to_vec());

        let events = vec![
            // Remove at height 200 fed FIRST
            synthetic_event(200, "b", &[(BK_CHANGE_REMOVED, 2u16, [0u8; 96])]),
            // Add at height 100 fed SECOND
            synthetic_event(100, "a", &[(BK_CHANGE_ADDED, 2u16, pk2)]),
        ];
        let result = fold_bk_updates(genesis, &events).expect("fold succeeds");
        // Correct chronological application: add(2) then remove(2) = {0} only.
        assert_eq!(result.len(), 1);
        assert!(result.contains_key(&0));
        assert!(!result.contains_key(&2));
    }

    #[test]
    fn fold_uses_chain_order_as_tiebreaker_at_same_height() {
        // Two events at same height: add(2) with chain_order "a", remove(2) with "b".
        // Sorted, add comes first, then remove — result excludes 2.
        let pk0 = uncompressed_pk(1);
        let pk2 = uncompressed_pk(3);
        let mut genesis = HashMap::new();
        genesis.insert(0u16, pk0.to_vec());

        let events = vec![
            // Feed remove first to force the sort to matter.
            synthetic_event(500, "b", &[(BK_CHANGE_REMOVED, 2u16, [0u8; 96])]),
            synthetic_event(500, "a", &[(BK_CHANGE_ADDED, 2u16, pk2)]),
        ];
        let result = fold_bk_updates(genesis, &events).expect("fold succeeds");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key(&0));
    }

    #[test]
    fn fold_rejects_empty_result() {
        // Genesis has one signer, event removes it → fold must error rather
        // than return an empty (unusable) BK set.
        let pk0 = uncompressed_pk(1);
        let mut genesis = HashMap::new();
        genesis.insert(0u16, pk0.to_vec());
        let events = vec![synthetic_event(
            100,
            "a",
            &[(BK_CHANGE_REMOVED, 0u16, [0u8; 96])],
        )];
        let err = fold_bk_updates(genesis, &events).expect_err("empty result must fail");
        let msg = err.to_string();
        assert!(msg.contains("empty"), "expected 'empty' in error: {msg}");
    }
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
