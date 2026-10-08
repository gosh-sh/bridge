//! Whether the aggregator's key for a withdrawal is already in the pk cache,
//! so that stage 5 writes none.
//!
//! Its slot is named by a hash over the Circuit-4 verifying key (through the
//! inner snark's protocol), the SRS and the aggregator's own configuration.
//! This crate cannot compute that before stage 5 — the inner snark does not
//! exist yet, and the aggregator is built on another halo2 fork. So it is
//! not predicted: `aggregate-proof` names the slot it used ([`LAST_SLOT`]),
//! and after a proof succeeds [`record`] notes that slot beside what the
//! proof ran with — the Circuit-4 proving key's digest and the
//! `aggregate-proof` binary's, as preflight verified them. A later preflight
//! that finds the same two digests, and that slot still published, knows the
//! next proof loads its key instead of generating it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The note [`record`] leaves in the pk cache.
pub(crate) const MARKER: &str = ".withdraw-slot.json";

/// The layout of [`Marker`]; a marker of another one vouches for nothing.
const MARKER_VERSION: u32 = 1;

/// Name prefix of a withdrawal aggregator slot (`--name` in the aggregator
/// call, then `__v3__<hash>`).
const SLOT_PREFIX: &str = "BridgeWithdrawalAggregatorVerifier__";

/// Where `aggregate-proof` names the withdrawal slot it last loaded or
/// published: `<name>` + `LAST_SLOT_SUFFIX` in
/// `bridge-evm-aggregator/src/aggregator_cache.rs`.
pub(crate) const LAST_SLOT: &str = "BridgeWithdrawalAggregatorVerifier.last-slot";

/// The field of a slot's `.meta.json` that records its key's length
/// (`CachedMeta` in `bridge-evm-aggregator/src/aggregator_cache.rs`).
pub(crate) const META_KEY_LENGTH: &str = "pk_bytes";

/// What a withdrawal proof runs with, as far as its aggregator slot depends
/// on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProofInputs {
    /// SHA-256 of the Circuit-4 proving key.
    pub(crate) event_pk_sha256: String,
    /// SHA-256 of the `aggregate-proof` binary.
    pub(crate) aggregate_proof_sha256: String,
}

/// What the last successful proof through this cache ran with, and the slot
/// it used.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    event_pk_sha256: String,
    aggregate_proof_sha256: String,
    /// The slot's stem.
    slot: String,
}

/// Whether the slot `stem` in `pk_cache_dir` is published: its key, and its
/// meta beside it (the aggregator publishes the meta last).
fn is_published(pk_cache_dir: &Path, stem: &str) -> bool {
    pk_cache_dir.join(format!("{stem}.pk")).is_file()
        && pk_cache_dir.join(format!("{stem}.meta.json")).is_file()
}

/// The withdrawal slot `aggregate-proof` last named, by stem.
fn reported_slot(pk_cache_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(pk_cache_dir.join(LAST_SLOT)).ok()?;
    let stem = text.trim();
    (stem.starts_with(SLOT_PREFIX) && !stem.contains('/')).then(|| stem.to_string())
}

/// Before a proof: drop the name of the slot the last one used, so that only
/// this proof's `aggregate-proof` can name one — one too old to report a
/// slot leaves none, and nothing is noted.
pub(crate) fn forget_reported_slot(pk_cache_dir: &Path) {
    let path = pk_cache_dir.join(LAST_SLOT);
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not clear the aggregator's last slot; this proof notes none",
            );
        }
    }
}

/// Note, after a successful proof, what it ran with and the slot
/// `aggregate-proof` named. Written to a temporary name and renamed, so a
/// reader never sees half of it.
pub(crate) fn record(pk_cache_dir: &Path, inputs: &ProofInputs) -> std::io::Result<()> {
    let slot = reported_slot(pk_cache_dir).ok_or_else(|| {
        std::io::Error::other(format!(
            "aggregate-proof named no withdrawal slot in {LAST_SLOT}"
        ))
    })?;
    let marker = Marker {
        version: MARKER_VERSION,
        event_pk_sha256: inputs.event_pk_sha256.clone(),
        aggregate_proof_sha256: inputs.aggregate_proof_sha256.clone(),
        slot,
    };
    let text = serde_json::to_string_pretty(&marker).map_err(std::io::Error::other)?;
    let tmp = pk_cache_dir.join(format!("{MARKER}.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, pk_cache_dir.join(MARKER))
}

/// The inputs a stage-5 proof ran with. `verified_event_pk` is the
/// Circuit-4 key's digest as preflight streamed it — stage 5 re-checks that
/// key right before the proof — and `None` on a run that generated the key
/// itself, whose manifest records it now. `aggregate_proof_sha256` is the
/// binary preflight hashed: one replaced since is not the one noted, and
/// the next preflight sees the difference.
pub(crate) fn proof_inputs(
    verified_event_pk: Option<&str>,
    params_dir: &Path,
    aggregate_proof_sha256: Option<&str>,
) -> Option<ProofInputs> {
    let event_pk_sha256 = match verified_event_pk {
        Some(digest) => digest.to_string(),
        None => bridge_prover_lib::keys::event_pk_recorded_digest(params_dir)?.1,
    };
    Some(ProofInputs {
        event_pk_sha256,
        aggregate_proof_sha256: aggregate_proof_sha256?.to_string(),
    })
}

/// [`record`] for stage 5, which has finished its proof. A failure, or
/// inputs nobody verified, only cost the next preflight its shortcut, so
/// neither is more than a warning.
pub(crate) fn note_proof(pk_cache_dir: &Path, inputs: Option<&ProofInputs>) {
    let Some(inputs) = inputs else {
        tracing::warn!(
            "could not tell which Circuit-4 key and aggregate-proof this proof ran with; the next \
             withdrawal's preflight reserves room for the aggregator key again",
        );
        return;
    };
    if let Err(e) = record(pk_cache_dir, inputs) {
        tracing::warn!(
            pk_cache_dir = %pk_cache_dir.display(),
            error = %e,
            "could not note the aggregator key this proof used; the next withdrawal's \
             preflight reserves room for it again",
        );
    }
}

/// Whether stage 5 will load the aggregator's key rather than generate it:
/// the last proof through `pk_cache_dir` ran with these `inputs` (`None`
/// when preflight could not verify them), and the slot it used is still
/// published.
pub(crate) fn is_cached(pk_cache_dir: &Path, inputs: Option<&ProofInputs>) -> bool {
    let Some(inputs) = inputs else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(pk_cache_dir.join(MARKER)) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Marker>(&text) else {
        return false;
    };
    marker.version == MARKER_VERSION
        && marker.event_pk_sha256 == inputs.event_pk_sha256
        && marker.aggregate_proof_sha256 == inputs.aggregate_proof_sha256
        && is_published(pk_cache_dir, &marker.slot)
}

/// A published withdrawal slot the aggregator would refuse to read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DamagedSlot {
    /// The slot's `.pk`.
    pub(crate) key: PathBuf,
    /// What is wrong with it.
    pub(crate) why: String,
}

/// The published withdrawal slots in `pk_cache_dir` whose meta is not JSON,
/// or whose key is not the length the meta records — a copy cut short, a
/// filesystem that acknowledged a sync it did not do. Stage 5 would refuse
/// either, after the burn, if its proof needed that slot. Slots an older
/// `aggregate-proof` published record no length, and pass.
pub(crate) fn damaged_slots(pk_cache_dir: &Path) -> Vec<DamagedSlot> {
    let Ok(entries) = std::fs::read_dir(pk_cache_dir) else {
        return Vec::new();
    };
    let mut damaged: Vec<DamagedSlot> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let stem = name.strip_suffix(".pk")?;
            (stem.starts_with(SLOT_PREFIX) && is_published(pk_cache_dir, stem))
                .then(|| stem.to_string())
        })
        .filter_map(|stem| {
            let key = pk_cache_dir.join(format!("{stem}.pk"));
            let meta = pk_cache_dir.join(format!("{stem}.meta.json"));
            let why = slot_damage(&key, &meta)?;
            Some(DamagedSlot {
                key,
                why,
            })
        })
        .collect();
    damaged.sort_by(|a, b| a.key.cmp(&b.key));
    damaged
}

/// What is wrong with the slot of `key` and `meta`, if anything is known to
/// be. Files that cannot be read are left to stage 5: a permission is not
/// damage.
fn slot_damage(key: &Path, meta: &Path) -> Option<String> {
    let text = std::fs::read_to_string(meta).ok()?;
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Some("its .meta.json is not whole".to_string());
    };
    let recorded = meta.get(META_KEY_LENGTH)?.as_u64()?;
    let bytes = std::fs::metadata(key).ok()?.len();
    (bytes != recorded).then(|| format!("{bytes} bytes, and its .meta.json records {recorded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: &str = "BridgeWithdrawalAggregatorVerifier__v3__0123abcd";

    fn inputs() -> ProofInputs {
        ProofInputs {
            event_pk_sha256: "aa11".to_string(),
            aggregate_proof_sha256: "cc33".to_string(),
        }
    }

    /// A pk cache with one published withdrawal slot, which
    /// `aggregate-proof` has just named.
    fn cache() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.pk")), b"key").unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.meta.json")), b"{}").unwrap();
        std::fs::write(dir.path().join(LAST_SLOT), SLOT).unwrap();
        dir
    }

    #[test]
    fn the_slot_a_proof_used_is_known_to_the_next_preflight() {
        let dir = cache();
        assert!(
            !is_cached(dir.path(), Some(&inputs())),
            "nothing recorded yet"
        );
        record(dir.path(), &inputs()).unwrap();
        assert!(is_cached(dir.path(), Some(&inputs())));
    }

    #[test]
    fn another_circuit_4_key_needs_another_slot() {
        let dir = cache();
        record(dir.path(), &inputs()).unwrap();
        let other = ProofInputs {
            event_pk_sha256: "bb22".to_string(),
            ..inputs()
        };
        assert!(!is_cached(dir.path(), Some(&other)));
        assert!(
            !is_cached(dir.path(), None),
            "inputs preflight did not verify vouch for nothing"
        );
    }

    #[test]
    fn another_aggregate_proof_may_name_another_slot() {
        let dir = cache();
        record(dir.path(), &inputs()).unwrap();
        let other = ProofInputs {
            aggregate_proof_sha256: "dd44".to_string(),
            ..inputs()
        };
        assert!(!is_cached(dir.path(), Some(&other)));
    }

    #[test]
    fn a_slot_removed_since_needs_its_room_again() {
        for gone in [".pk", ".meta.json"] {
            let dir = cache();
            record(dir.path(), &inputs()).unwrap();
            std::fs::remove_file(dir.path().join(format!("{SLOT}{gone}"))).unwrap();
            assert!(!is_cached(dir.path(), Some(&inputs())), "{gone} removed");
        }
    }

    #[test]
    fn only_the_slot_the_proof_used_is_noted() {
        // The previous version's slot, removed after the first withdrawal
        // with the new one as the README says, takes nothing with it.
        let dir = cache();
        let old = "BridgeWithdrawalAggregatorVerifier__v3__ffff";
        std::fs::write(dir.path().join(format!("{old}.pk")), b"old key").unwrap();
        std::fs::write(dir.path().join(format!("{old}.meta.json")), b"{}").unwrap();
        record(dir.path(), &inputs()).unwrap();
        std::fs::remove_file(dir.path().join(format!("{old}.pk"))).unwrap();
        std::fs::remove_file(dir.path().join(format!("{old}.meta.json"))).unwrap();
        assert!(is_cached(dir.path(), Some(&inputs())));
    }

    #[test]
    fn a_proof_whose_aggregator_named_no_slot_vouches_for_nothing() {
        // An `aggregate-proof` too old to name its slot, or a name a
        // previous proof left: stage 5 forgets it before proving.
        let dir = cache();
        forget_reported_slot(dir.path());
        record(dir.path(), &inputs()).expect_err("no slot named");
        assert!(!is_cached(dir.path(), Some(&inputs())));
        std::fs::write(dir.path().join(LAST_SLOT), "../elsewhere").unwrap();
        record(dir.path(), &inputs()).expect_err("not a withdrawal slot");
    }

    #[test]
    fn a_marker_of_another_layout_vouches_for_nothing() {
        let dir = cache();
        record(dir.path(), &inputs()).unwrap();
        let path = dir.path().join(MARKER);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(!is_cached(dir.path(), Some(&inputs())), "another version");
        std::fs::write(&path, text.replacen('{', "{\n  \"slots\": [],", 1)).unwrap();
        assert!(!is_cached(dir.path(), Some(&inputs())), "an unknown field");
    }

    #[test]
    fn a_stage_5_proof_is_noted_with_the_inputs_preflight_verified() {
        let dir = cache();
        let params = tempfile::tempdir().unwrap();
        let noted = proof_inputs(Some("aa11"), params.path(), Some("cc33")).unwrap();
        assert_eq!(noted, inputs());
        note_proof(dir.path(), Some(&noted));
        assert!(is_cached(dir.path(), Some(&inputs())));
        assert_eq!(
            proof_inputs(Some("aa11"), params.path(), None),
            None,
            "an aggregate-proof preflight could not hash vouches for nothing"
        );
        assert_eq!(
            proof_inputs(None, params.path(), Some("cc33")),
            None,
            "nor a Circuit-4 key with no manifest"
        );
    }

    #[test]
    fn a_key_shorter_than_its_meta_records_is_found_before_the_burn() {
        let dir = cache();
        let meta = dir.path().join(format!("{SLOT}.meta.json"));
        std::fs::write(&meta, format!("{{\"{META_KEY_LENGTH}\": 3}}")).unwrap();
        assert!(damaged_slots(dir.path()).is_empty(), "3 bytes, as recorded");
        std::fs::write(&meta, format!("{{\"{META_KEY_LENGTH}\": 6846416031}}")).unwrap();
        assert_eq!(damaged_slots(dir.path()), vec![DamagedSlot {
            key: dir.path().join(format!("{SLOT}.pk")),
            why: "3 bytes, and its .meta.json records 6846416031".to_string(),
        }]);
        std::fs::write(&meta, b"{\"break_po").unwrap();
        assert_eq!(damaged_slots(dir.path()).len(), 1, "a meta cut short");
        std::fs::write(&meta, b"{\"break_points\": []}").unwrap();
        assert!(
            damaged_slots(dir.path()).is_empty(),
            "an older aggregate-proof's slot records no length"
        );
    }
}
