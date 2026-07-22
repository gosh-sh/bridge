//! Fetch and reconstruct the Block Keeper (BK) set from a node's GraphQL API.

use std::collections::HashMap;

use anyhow::{bail, Context};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use tracing::{debug, info};

use crate::gql_client::{BkSetUpdateWithAttestations, GqlClient};

/// Fetch the current BK set from the node's GraphQL `bkSetUpdates`.
///
/// **DISABLED (2026-07-22).** This function is architecturally broken:
/// it replays the `bkSetUpdates` delta log starting from an empty map, but
/// AN's protocol does not emit the genesis committee as a synthetic `Added`
/// event. On any rotating chain this returns the diff-from-genesis (not the
/// current active set); on a fresh chain it returns empty. Callers now load
/// the genesis BK set from a JSON config; for cold-start against a distant
/// block on a long-running rotating chain, use `bk_set_at_height` (to be
/// added — see `TECHNICAL_README.md` § "BK-set bootstrap").
///
/// Kept in-tree behind a never-active cfg so historical references in docs
/// and imports remain compilable; delete once `bk_set_at_height` lands and
/// docs are cleaned up.
#[cfg(any())]
pub async fn fetch_bk_set(client: &GqlClient) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    // Query both first (genesis-era) and last (recent) bkSetUpdates to capture
    // the full history of adds/removes. Use the light query (no attestation subfields)
    // to avoid database timeouts on large networks.
    let mut updates = client.query_bk_set_updates_light(500, true).await?;
    let recent = client.query_bk_set_updates_light(500, false).await?;
    // Merge, deduplicating by block_id.
    let existing_ids: std::collections::HashSet<String> =
        updates.iter().map(|u| u.block_id.clone()).collect();
    for u in recent {
        if !existing_ids.contains(&u.block_id) {
            updates.push(u);
        }
    }
    if updates.is_empty() {
        bail!("no bkSetUpdates found — node may not have produced blocks yet");
    }

    let mut bk_set: HashMap<u16, Vec<u8>> = HashMap::new();
    let mut total_adds = 0usize;
    let mut total_removes = 0usize;

    for update in &updates {
        if update.bk_set_update_hex.is_empty() {
            continue;
        }
        let blob = hex::decode(&update.bk_set_update_hex)
            .context("failed to decode bk_set_update hex")?;

        let changes = parse_bk_set_changes(&blob);
        for (variant, signer_idx, pk) in &changes {
            match *variant {
                BK_CHANGE_ADDED => {
                    info!(
                        "  height={:?}: Added signer {} pk={}...",
                        update.height,
                        signer_idx,
                        hex::encode(&pk[..8])
                    );
                    bk_set.insert(*signer_idx, pk.clone());
                    total_adds += 1;
                }
                BK_CHANGE_REMOVED => {
                    info!(
                        "  height={:?}: Removed signer {}",
                        update.height,
                        signer_idx,
                    );
                    bk_set.remove(signer_idx);
                    total_removes += 1;
                }
                _ => {}
            }
        }
    }

    info!(
        "processed {} bkSetUpdates: {} adds, {} removes, {} active signers",
        updates.len(),
        total_adds,
        total_removes,
        bk_set.len()
    );

    if bk_set.is_empty() {
        bail!(
            "BK set is empty after processing {} bkSetUpdates ({} adds, {} removes). \
             The initial BK set may have been established at genesis and not captured \
             in bkSetUpdates. Use a bk_set.json config file as fallback.",
            updates.len(),
            total_adds,
            total_removes,
        );
    }

    let bk_set = normalize_bk_set_pubkeys(bk_set)?;
    info!(
        "extracted BK set with {} signers: {:?}",
        bk_set.len(),
        bk_set.keys().collect::<Vec<_>>()
    );
    Ok(bk_set)
}

/// How many recent `bkSetUpdates` to pull when scanning for the next event
/// past a cursor. Sized to comfortably cover the prover's worst-case lag:
/// shellnet bursts are bounded at ~5 events, and the prover normally lags by
/// at most a few `W·P` windows, so 100 is far more than enough.
const NEXT_UPDATE_AFTER_LOOKBACK: u32 = 100;

/// Cursor-walk over `bkSetUpdates`: returns the *next* rotation event whose
/// chain height is strictly greater than `cursor_seq_no`, regardless of W·P
/// alignment. Returns `Ok(None)` when the chain has no rotation past the
/// cursor yet (i.e. the prover is caught up on rotations).
///
/// This is the missing-detector that the Phase-1 L2≠L3-at-thinned-key-block
/// check could not provide — real rotations land on arbitrary heights, almost
/// none of them W·P-aligned.
///
/// The returned struct includes `block_id`, `height`, and the raw
/// `bk_set_update_hex` blob; the caller parses the latter via
/// [`parse_bk_set_changes_pub`] to derive the new pubkey table.
pub async fn next_update_after(
    client: &GqlClient,
    cursor_seq_no: u64,
) -> anyhow::Result<Option<BkSetUpdateWithAttestations>> {
    let recent = client
        .query_bk_set_updates_light(NEXT_UPDATE_AFTER_LOOKBACK, false)
        .await
        .context("query_bk_set_updates_light failed in next_update_after")?;

    // Filter to events strictly past cursor, then pick the smallest height
    // (so we drain in chronological order).
    let next = recent
        .into_iter()
        .filter(|u| u.height.map(|h| h > cursor_seq_no).unwrap_or(false))
        .min_by_key(|u| u.height.unwrap_or(u64::MAX));

    Ok(next)
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
/// Format: `{ "0": "hex-pubkey", "1": "hex-pubkey", ... }`
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
}
