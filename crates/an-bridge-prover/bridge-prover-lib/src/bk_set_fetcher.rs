//! Fetch and reconstruct the Block Keeper (BK) set from a node's GraphQL API.

use std::collections::HashMap;

use anyhow::{bail, Context};
use halo2_base::halo2_proofs::halo2curves::bls12_381::G1Affine;
use tracing::info;

use crate::gql_client::{BkSetUpdateWithAttestations, GqlClient};

/// Fetch the current BK set from the node's GraphQL `bkSetUpdates`.
///
/// Queries the full history of BK set changes (adds/removes) and reconstructs
/// the current active validator set. Returns a map of signer_index -> 48-byte
/// compressed BLS pubkey.
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
mod live_tests {
    //! Live-network tests — ignored by default. Run with:
    //!   cargo test -p bridge-prover-lib --release -- --ignored next_update_after
    use super::*;
    use crate::gql_client::GqlClient;

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
