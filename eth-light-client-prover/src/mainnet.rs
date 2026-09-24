//! Runtime loader for mainnet `LightClientUpdate` fixtures on the **rotate** path.
//!
//! Both the shard-proof emitter (`examples/export_shard_snark.rs`) and the N=8
//! recursion-tree driver (`examples/rotate_tree_n8.rs`) must glue over the **same**
//! committee, so the shard proofs' exposed subtree roots line up with the tree
//! root's `RotateGlueWitness`. This module is the single source: it parses the
//! `next_sync_committee` (512 pubkeys + `aggregate_pubkey`), its
//! `next_sync_committee_branch` (6 nodes @ gindex 87), the attested `state_root`,
//! and the `signature_slot` from a beacon `LightClientUpdate` JSON — the exact
//! shape `tests/rotate_mock_prover.rs` validates against live data.

use std::path::{Path, PathBuf};

use crate::committee::{COMMITTEE_SHARDS, PUBKEYS_PER_SHARD, SYNC_COMMITTEE_SIZE};

/// 32 slots/epoch × 256 epochs/period.
pub const SLOTS_PER_SYNC_PERIOD: u64 = 32 * 256;

/// A parsed mainnet rotate update: the `next_sync_committee` + the SSZ proof that
/// binds it to a beacon `state_root`.
#[derive(Clone, Debug)]
pub struct RotateFixture {
    /// 512 committee pubkeys (48 compressed bytes each).
    pub pubkeys: Vec<[u8; 48]>,
    /// `aggregate_pubkey` (48 compressed bytes).
    pub aggregate: [u8; 48],
    /// `next_sync_committee_branch` — 6 nodes for Electra/Fulu gindex 87.
    pub branch: Vec<[u8; 32]>,
    /// Attested beacon `state_root` the branch reconstructs.
    pub state_root: [u8; 32],
    /// `signature_slot` of the update.
    pub signature_slot: u64,
}

impl RotateFixture {
    /// Sync-committee period the update rotates INTO (`signature_slot / 8192`).
    pub fn period(&self) -> u64 {
        self.signature_slot / SLOTS_PER_SYNC_PERIOD
    }
}

fn hexv(s: &str) -> anyhow::Result<Vec<u8>> {
    Ok(hex::decode(s.trim_start_matches("0x"))?)
}

fn h32(s: &str) -> anyhow::Result<[u8; 32]> {
    hexv(s)?.try_into().map_err(|_| anyhow::anyhow!("expected 32-byte hex"))
}

fn pk48(s: &str) -> anyhow::Result<[u8; 48]> {
    hexv(s)?.try_into().map_err(|_| anyhow::anyhow!("expected 48-byte hex"))
}

/// Parse a beacon `LightClientUpdate` JSON (array-wrapped or `{data:…}`), as
/// written by `scripts/fetch_lc_fixtures.sh` / the beacon API.
pub fn load_rotate_update(path: &Path) -> anyhow::Result<RotateFixture> {
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = if raw.is_array() { raw[0]["data"].clone() } else { raw["data"].clone() };

    let nsc = &d["next_sync_committee"];
    let pubkeys: Vec<[u8; 48]> = nsc["pubkeys"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("next_sync_committee.pubkeys missing"))?
        .iter()
        .map(|p| pk48(p.as_str().unwrap_or_default()))
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        pubkeys.len() == SYNC_COMMITTEE_SIZE,
        "expected {SYNC_COMMITTEE_SIZE} pubkeys, got {}",
        pubkeys.len()
    );
    let aggregate = pk48(nsc["aggregate_pubkey"].as_str().unwrap_or_default())?;

    let branch: Vec<[u8; 32]> = d["next_sync_committee_branch"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("next_sync_committee_branch missing"))?
        .iter()
        .map(|b| h32(b.as_str().unwrap_or_default()))
        .collect::<anyhow::Result<_>>()?;

    let state_root = h32(
        d["attested_header"]["beacon"]["state_root"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("attested_header.beacon.state_root missing"))?,
    )?;

    let slot_v = &d["signature_slot"];
    let signature_slot = slot_v
        .as_u64()
        .or_else(|| slot_v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| anyhow::anyhow!("signature_slot missing/unparseable"))?;

    Ok(RotateFixture { pubkeys, aggregate, branch, state_root, signature_slot })
}

/// Locate a committed mainnet update fixture (`fixtures/mainnet/update_period_*.json`).
pub fn find_update_fixture() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mainnet");
    std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("update_period_") && n.ends_with(".json"))
    })
}

/// The committee source both the shard emitter and the tree driver share, so the
/// shard proofs' exposed subtree roots line up with the tree root's witness.
#[derive(Clone, Debug)]
pub struct CommitteeSource {
    /// `COMMITTEE_SHARDS` balanced 64-pubkey slices (proof order).
    pub shards: Vec<Vec<[u8; 48]>>,
    /// `aggregate_pubkey`.
    pub aggregate: [u8; 48],
    /// The parsed update when the source is a real fixture (branch + state_root).
    pub update: Option<RotateFixture>,
    /// Human-readable provenance.
    pub label: String,
}

/// Deterministic synthetic distinct committee (fallback when no fixture is present).
/// Shard 0 is byte-identical to the legacy single-shard witness
/// (`(i*7 + j*13 + 1) % 251`), so the N=1 examples still line up.
pub fn synthetic_committee() -> CommitteeSource {
    let shards = (0..COMMITTEE_SHARDS)
        .map(|s| {
            (0..PUBKEYS_PER_SHARD)
                .map(|i| {
                    let mut pk = [0u8; 48];
                    for (j, b) in pk.iter_mut().enumerate() {
                        *b = ((s * 64 + i * 7 + j * 13 + 1) % 251) as u8;
                    }
                    pk
                })
                .collect()
        })
        .collect();
    CommitteeSource {
        shards,
        aggregate: [0xABu8; 48],
        update: None,
        label: "synthetic distinct".to_string(),
    }
}

/// Prefer the real mainnet `next_sync_committee` (split into `COMMITTEE_SHARDS`
/// 64-pubkey slices); fall back to [`synthetic_committee`].
pub fn committee_source() -> CommitteeSource {
    if let Some(path) = find_update_fixture() {
        match load_rotate_update(&path) {
            Ok(fx) => {
                let shards =
                    fx.pubkeys.chunks(PUBKEYS_PER_SHARD).map(|c| c.to_vec()).collect::<Vec<_>>();
                let label = format!("mainnet {}", path.file_name().unwrap().to_string_lossy());
                return CommitteeSource {
                    shards,
                    aggregate: fx.aggregate,
                    update: Some(fx),
                    label,
                };
            }
            Err(e) => eprintln!("WARN: update fixture unusable ({e}); using synthetic distinct"),
        }
    }
    synthetic_committee()
}
