//! Whether the aggregator's key for a withdrawal is already in the pk cache,
//! so that stage 5 writes none.
//!
//! Its slot is named by a hash over the Circuit-4 verifying key (through the
//! inner snark's protocol), the SRS and the aggregator's own configuration.
//! This crate cannot compute that before stage 5 — the inner snark does not
//! exist yet, and the aggregator is built on another halo2 fork. So it is
//! not predicted: after a proof succeeds, [`record`] notes what the proof
//! ran with — the Circuit-4 proving key's digest and the `aggregate-proof`
//! binary's — and the withdrawal slots published then. A later preflight
//! that finds the same two digests, and every one of those slots still
//! published, knows the next proof loads its key instead of generating it.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The note [`record`] leaves in the pk cache.
pub(crate) const MARKER: &str = ".withdraw-slot.json";

/// Name prefix of a withdrawal aggregator slot (`--name` in the aggregator
/// call, then `__v3__<hash>`).
const SLOT_PREFIX: &str = "BridgeWithdrawalAggregatorVerifier__";

/// What the last successful proof through this cache ran with.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    /// SHA-256 of the Circuit-4 proving key, as its manifest records it.
    event_pk_sha256: String,
    /// SHA-256 of the `aggregate-proof` binary.
    aggregate_proof_sha256: String,
    /// The withdrawal slots published after the proof, one of which it used.
    slots: Vec<String>,
}

/// The published withdrawal slots in `pk_cache_dir`, by key file name,
/// sorted: a `.pk` whose `.meta.json` is beside it (the aggregator publishes
/// the meta last).
fn published_slots(pk_cache_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(pk_cache_dir) else {
        return Vec::new();
    };
    let mut slots: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix(SLOT_PREFIX)
                .and_then(|rest| rest.strip_suffix(".pk"))
                .is_some_and(|hash| !hash.is_empty() && is_published(pk_cache_dir, name))
        })
        .collect();
    slots.sort();
    slots
}

/// Whether the key `pk_name` in `pk_cache_dir` has its meta beside it.
fn is_published(pk_cache_dir: &Path, pk_name: &str) -> bool {
    let stem = pk_name.strip_suffix(".pk").unwrap_or(pk_name);
    pk_cache_dir.join(pk_name).is_file() && pk_cache_dir.join(format!("{stem}.meta.json")).is_file()
}

/// Note, after a successful proof, what it ran with. Written to a temporary
/// name and renamed, so a reader never sees half of it.
pub(crate) fn record(
    pk_cache_dir: &Path,
    event_pk_sha256: &str,
    aggregate_proof: &Path,
) -> std::io::Result<()> {
    let marker = Marker {
        event_pk_sha256: event_pk_sha256.to_string(),
        aggregate_proof_sha256: bridge_prover_lib::keys::sha256_of(aggregate_proof)?,
        slots: published_slots(pk_cache_dir),
    };
    let text = serde_json::to_string_pretty(&marker).map_err(std::io::Error::other)?;
    let tmp = pk_cache_dir.join(format!("{MARKER}.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, pk_cache_dir.join(MARKER))
}

/// [`record`] for stage 5, which has finished its proof: the digests come
/// from the directories, and a failure only costs the next preflight its
/// shortcut, so it is a warning.
pub(crate) fn note_proof(pk_cache_dir: &Path, params_dir: &Path, aggregator_dir: &Path) {
    let Some((_, event_pk_sha256)) = bridge_prover_lib::keys::event_pk_recorded_digest(params_dir)
    else {
        return;
    };
    let aggregate_proof = crate::preflight::aggregate_proof_bin(aggregator_dir);
    if let Err(e) = record(pk_cache_dir, &event_pk_sha256, &aggregate_proof) {
        tracing::warn!(
            pk_cache_dir = %pk_cache_dir.display(),
            error = %e,
            "could not note the aggregator key this proof used; the next withdrawal's \
             preflight reserves room for it again",
        );
    }
}

/// Whether stage 5 will load the aggregator's key rather than generate it:
/// the last proof through `pk_cache_dir` ran with this Circuit-4 key
/// (`event_pk_sha256`, `None` when preflight did not verify one) and this
/// `aggregate-proof`, and every withdrawal slot published then still is.
pub(crate) fn is_cached(
    pk_cache_dir: &Path,
    event_pk_sha256: Option<&str>,
    aggregate_proof: &Path,
) -> bool {
    let Some(event_pk_sha256) = event_pk_sha256 else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(pk_cache_dir.join(MARKER)) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<Marker>(&text) else {
        return false;
    };
    let Ok(aggregate_proof_sha256) = bridge_prover_lib::keys::sha256_of(aggregate_proof) else {
        return false;
    };
    marker.event_pk_sha256 == event_pk_sha256
        && marker.aggregate_proof_sha256 == aggregate_proof_sha256
        && !marker.slots.is_empty()
        && marker
            .slots
            .iter()
            .all(|slot| is_published(pk_cache_dir, slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aa11";
    const SLOT: &str = "BridgeWithdrawalAggregatorVerifier__v3__0123abcd";

    /// A pk cache with one published withdrawal slot, and an
    /// `aggregate-proof` binary beside it.
    fn cache() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.pk")), b"key").unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.meta.json")), b"{}").unwrap();
        let bin = dir.path().join("aggregate-proof");
        std::fs::write(&bin, b"aggregator v1").unwrap();
        (dir, bin)
    }

    #[test]
    fn the_slot_a_proof_used_is_known_to_the_next_preflight() {
        let (dir, bin) = cache();
        assert!(
            !is_cached(dir.path(), Some(KEY), &bin),
            "nothing recorded yet"
        );
        record(dir.path(), KEY, &bin).unwrap();
        assert!(is_cached(dir.path(), Some(KEY), &bin));
    }

    #[test]
    fn another_circuit_4_key_needs_another_slot() {
        let (dir, bin) = cache();
        record(dir.path(), KEY, &bin).unwrap();
        assert!(!is_cached(dir.path(), Some("bb22"), &bin));
        assert!(
            !is_cached(dir.path(), None, &bin),
            "a key preflight did not verify vouches for nothing"
        );
    }

    #[test]
    fn another_aggregate_proof_may_name_another_slot() {
        let (dir, bin) = cache();
        record(dir.path(), KEY, &bin).unwrap();
        std::fs::write(&bin, b"aggregator v2").unwrap();
        assert!(!is_cached(dir.path(), Some(KEY), &bin));
    }

    #[test]
    fn a_slot_removed_since_needs_its_room_again() {
        let (dir, bin) = cache();
        record(dir.path(), KEY, &bin).unwrap();
        std::fs::remove_file(dir.path().join(format!("{SLOT}.meta.json"))).unwrap();
        assert!(!is_cached(dir.path(), Some(KEY), &bin));
    }

    #[test]
    fn a_proof_that_left_no_slot_vouches_for_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("aggregate-proof");
        std::fs::write(&bin, b"aggregator v1").unwrap();
        record(dir.path(), KEY, &bin).unwrap();
        assert!(!is_cached(dir.path(), Some(KEY), &bin));
    }

    #[test]
    fn only_published_withdrawal_slots_are_listed() {
        let (dir, _) = cache();
        for name in [
            format!("{SLOT}.pk.partial-7-0a1b2c3d"),
            "BridgeWithdrawalAggregatorVerifier__v3__ffff.pk".to_string(),
            "BridgeBlockAggregatorVerifier__v3__eeee.pk".to_string(),
            "BridgeBlockAggregatorVerifier__v3__eeee.meta.json".to_string(),
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        assert_eq!(published_slots(dir.path()), vec![format!("{SLOT}.pk")]);
    }
}
