//! Whether the aggregator's key for a withdrawal is already in the pk cache,
//! so that stage 5 writes none.
//!
//! Its slot is named by a hash over the Circuit-4 verifying key (through the
//! inner snark's protocol), the SRS and the aggregator's own configuration.
//! This crate cannot compute that before stage 5 — the inner snark does not
//! exist yet, and the aggregator is built on another halo2 fork. So it is
//! not predicted: `aggregate-proof` names the slot it used in a file of this
//! run's own ([`report_path`], `--slot-report`), and after a proof succeeds
//! [`record`] notes that slot beside what the proof ran with — the Circuit-4
//! proving key's digest and the `aggregate-proof` binary's, as preflight
//! verified them. A later preflight that finds the same two digests, and
//! that slot still published, knows the next proof loads its key instead of
//! generating it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The note [`record`] leaves in the pk cache.
pub(crate) const MARKER: &str = ".withdraw-slot.json";

/// The layout of [`Marker`]; a marker of another one vouches for nothing.
const MARKER_VERSION: u32 = 1;

/// Name prefix of a withdrawal aggregator slot (`--name` in the aggregator
/// call, then `__v3__<hash>`).
const SLOT_PREFIX: &str = "BridgeWithdrawalAggregatorVerifier__";

/// Where stage 5 has `aggregate-proof` name the slot it used: in the
/// `--snark-dir`, which the prove lock keeps to one proof at a time — never
/// in the pk cache, which other processes share and sweep.
pub(crate) fn report_path(snark_dir: &Path) -> PathBuf {
    snark_dir.join("aggregator-slot")
}

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

/// The withdrawal slot `aggregate-proof` named in `report`, by stem.
fn reported_slot(report: &Path) -> Option<String> {
    let text = std::fs::read_to_string(report).ok()?;
    let stem = text.trim();
    (stem.starts_with(SLOT_PREFIX) && !stem.contains('/')).then(|| stem.to_string())
}

/// Before a proof: remove what an earlier proof left at `report`, so that
/// only this one's `aggregate-proof` can name a slot there. `false`: it
/// could not be removed, and this proof must not trust what it finds.
pub(crate) fn clear_report(report: &Path) -> bool {
    match std::fs::remove_file(report) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(
                path = %report.display(),
                error = %e,
                "could not clear the aggregator's slot report; this proof notes no slot",
            );
            false
        },
    }
}

/// Note, after a successful proof, what it ran with and the slot
/// `aggregate-proof` named in `report`. Written to a temporary name and
/// renamed, so a reader never sees half of it.
pub(crate) fn record(
    pk_cache_dir: &Path,
    report: &Path,
    inputs: &ProofInputs,
) -> std::io::Result<()> {
    let slot = reported_slot(report).ok_or_else(|| {
        std::io::Error::other(format!(
            "aggregate-proof named no withdrawal slot in {}",
            report.display()
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

/// [`record`] for stage 5, which has finished its proof. `report` is
/// `None` when it could not be cleared before the proof. A failure, or
/// inputs nobody verified, only cost the next preflight its shortcut, so
/// neither is more than a warning.
pub(crate) fn note_proof(pk_cache_dir: &Path, report: Option<&Path>, inputs: Option<&ProofInputs>) {
    let (Some(report), Some(inputs)) = (report, inputs) else {
        tracing::warn!(
            "could not tell which aggregator key, Circuit-4 key and aggregate-proof this proof \
             used; the next withdrawal's preflight reserves room for the aggregator key again",
        );
        return;
    };
    if let Err(e) = record(pk_cache_dir, report, inputs) {
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

/// What is wrong with a published withdrawal slot stage 5 would fail on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotProblem {
    /// Its bytes are wrong: the aggregator refuses it, and it has to go.
    Damaged(String),
    /// This user cannot read it: a permission to fix, not a file to remove.
    Unreadable(String),
}

/// A published withdrawal slot the aggregator would fail on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BadSlot {
    /// The slot's `.pk`.
    pub(crate) key: PathBuf,
    pub(crate) problem: SlotProblem,
}

/// The published withdrawal slots in `pk_cache_dir` stage 5 would fail on
/// if its proof needed them, after the burn: one this user cannot read, one
/// whose meta is not JSON, or whose key is not the length the meta records
/// — a copy cut short, a filesystem that acknowledged a sync it did not do.
/// A key of the right length with wrong bytes is not found here. Slots an
/// older `aggregate-proof` published record no length until their key has
/// read back once, and pass.
pub(crate) fn bad_slots(pk_cache_dir: &Path) -> Vec<BadSlot> {
    let Ok(entries) = std::fs::read_dir(pk_cache_dir) else {
        return Vec::new();
    };
    let mut bad: Vec<BadSlot> = entries
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
            let problem = slot_problem(&key, &meta)?;
            Some(BadSlot {
                key,
                problem,
            })
        })
        .collect();
    bad.sort_by(|a, b| a.key.cmp(&b.key));
    bad
}

/// What stage 5 would fail on in the slot of `key` and `meta`, if anything.
fn slot_problem(key: &Path, meta: &Path) -> Option<SlotProblem> {
    let bytes = match std::fs::read(meta) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Some(SlotProblem::Unreadable(format!(
                "its .meta.json cannot be read: {e}"
            )))
        },
    };
    let file = match std::fs::File::open(key) {
        Ok(file) => file,
        Err(e) => return Some(SlotProblem::Unreadable(format!("it cannot be read: {e}"))),
    };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Some(SlotProblem::Damaged(
            "its .meta.json is not whole".to_string(),
        ));
    };
    let recorded = match meta.get(META_KEY_LENGTH) {
        None | Some(serde_json::Value::Null) => return None,
        Some(v) => match v.as_u64() {
            Some(n) => n,
            None => {
                return Some(SlotProblem::Damaged(format!(
                    "its .meta.json records a length that is not one: {v}"
                )))
            },
        },
    };
    let len = file.metadata().ok()?.len();
    (len != recorded).then(|| {
        SlotProblem::Damaged(format!(
            "{len} bytes, and its .meta.json records {recorded}"
        ))
    })
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

    /// A pk cache with one published withdrawal slot, and a snark directory
    /// whose report names it.
    fn cache() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.pk")), b"key").unwrap();
        std::fs::write(dir.path().join(format!("{SLOT}.meta.json")), b"{}").unwrap();
        let snark = dir.path().join("snark");
        std::fs::create_dir(&snark).unwrap();
        let report = report_path(&snark);
        std::fs::write(&report, SLOT).unwrap();
        (dir, report)
    }

    #[test]
    fn the_slot_a_proof_used_is_known_to_the_next_preflight() {
        let (dir, report) = cache();
        assert!(
            !is_cached(dir.path(), Some(&inputs())),
            "nothing recorded yet"
        );
        record(dir.path(), &report, &inputs()).unwrap();
        assert!(is_cached(dir.path(), Some(&inputs())));
    }

    #[test]
    fn another_circuit_4_key_needs_another_slot() {
        let (dir, report) = cache();
        record(dir.path(), &report, &inputs()).unwrap();
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
        let (dir, report) = cache();
        record(dir.path(), &report, &inputs()).unwrap();
        let other = ProofInputs {
            aggregate_proof_sha256: "dd44".to_string(),
            ..inputs()
        };
        assert!(!is_cached(dir.path(), Some(&other)));
    }

    #[test]
    fn a_slot_removed_since_needs_its_room_again() {
        for gone in [".pk", ".meta.json"] {
            let (dir, report) = cache();
            record(dir.path(), &report, &inputs()).unwrap();
            std::fs::remove_file(dir.path().join(format!("{SLOT}{gone}"))).unwrap();
            assert!(!is_cached(dir.path(), Some(&inputs())), "{gone} removed");
        }
    }

    #[test]
    fn only_the_slot_the_proof_used_is_noted() {
        // The previous version's slot, removed after the first withdrawal
        // with the new one as the README says, takes nothing with it.
        let (dir, report) = cache();
        let old = "BridgeWithdrawalAggregatorVerifier__v3__ffff";
        std::fs::write(dir.path().join(format!("{old}.pk")), b"old key").unwrap();
        std::fs::write(dir.path().join(format!("{old}.meta.json")), b"{}").unwrap();
        record(dir.path(), &report, &inputs()).unwrap();
        std::fs::remove_file(dir.path().join(format!("{old}.pk"))).unwrap();
        std::fs::remove_file(dir.path().join(format!("{old}.meta.json"))).unwrap();
        assert!(is_cached(dir.path(), Some(&inputs())));
    }

    #[test]
    fn a_proof_whose_aggregator_named_no_slot_vouches_for_nothing() {
        // What an earlier proof left is cleared before this one; an
        // `aggregate-proof` that names nothing leaves nothing to note.
        let (dir, report) = cache();
        assert!(clear_report(&report));
        assert!(!report.exists());
        assert!(clear_report(&report), "nothing to clear is cleared");
        record(dir.path(), &report, &inputs()).expect_err("no slot named");
        assert!(!is_cached(dir.path(), Some(&inputs())));
        std::fs::write(&report, "../elsewhere").unwrap();
        record(dir.path(), &report, &inputs()).expect_err("not a withdrawal slot");
    }

    #[test]
    fn a_report_that_cannot_be_cleared_is_not_trusted() {
        // A directory where it stands: removing it fails, and what is
        // there may be an earlier proof's.
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("aggregator-slot");
        std::fs::create_dir(&report).unwrap();
        assert!(!clear_report(&report));
    }

    #[test]
    fn a_marker_of_another_layout_vouches_for_nothing() {
        let (dir, report) = cache();
        record(dir.path(), &report, &inputs()).unwrap();
        let path = dir.path().join(MARKER);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(!is_cached(dir.path(), Some(&inputs())), "another version");
        std::fs::write(&path, text.replacen('{', "{\n  \"slots\": [],", 1)).unwrap();
        assert!(!is_cached(dir.path(), Some(&inputs())), "an unknown field");
    }

    #[test]
    fn a_stage_5_proof_is_noted_only_with_its_report_and_its_inputs() {
        let (dir, report) = cache();
        note_proof(dir.path(), None, Some(&inputs()));
        assert!(!is_cached(dir.path(), Some(&inputs())), "no report");
        note_proof(dir.path(), Some(&report), None);
        assert!(!is_cached(dir.path(), Some(&inputs())), "no inputs");
        note_proof(dir.path(), Some(&report), Some(&inputs()));
        assert!(is_cached(dir.path(), Some(&inputs())));
    }

    #[test]
    fn a_key_shorter_than_its_meta_records_is_found_before_the_burn() {
        let (dir, _) = cache();
        let meta = dir.path().join(format!("{SLOT}.meta.json"));
        let damaged = |why: &str| {
            vec![BadSlot {
                key: dir.path().join(format!("{SLOT}.pk")),
                problem: SlotProblem::Damaged(why.to_string()),
            }]
        };
        std::fs::write(&meta, format!("{{\"{META_KEY_LENGTH}\": 3}}")).unwrap();
        assert!(bad_slots(dir.path()).is_empty(), "3 bytes, as recorded");
        std::fs::write(&meta, format!("{{\"{META_KEY_LENGTH}\": 6846416031}}")).unwrap();
        assert_eq!(
            bad_slots(dir.path()),
            damaged("3 bytes, and its .meta.json records 6846416031")
        );
        std::fs::write(&meta, b"{\"break_po").unwrap();
        assert_eq!(
            bad_slots(dir.path()),
            damaged("its .meta.json is not whole")
        );
        std::fs::write(&meta, [0xff, 0xfe, b'{']).unwrap();
        assert_eq!(
            bad_slots(dir.path()),
            damaged("its .meta.json is not whole"),
            "not UTF-8"
        );
        std::fs::write(&meta, b"{\"break_points\": []}").unwrap();
        assert!(
            bad_slots(dir.path()).is_empty(),
            "an older aggregate-proof's slot records no length"
        );
    }

    #[test]
    fn a_slot_this_user_cannot_read_is_found_before_the_burn() {
        // A container that writes the shared cache with umask 077.
        use std::os::unix::fs::PermissionsExt;
        let (dir, _) = cache();
        let key = dir.path().join(format!("{SLOT}.pk"));
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything: nothing to learn here then.
        if std::fs::File::open(&key).is_ok() {
            return;
        }
        let bad = bad_slots(dir.path());
        assert_eq!(bad.len(), 1);
        assert!(
            matches!(&bad[0].problem, SlotProblem::Unreadable(why) if why.contains("cannot be read")),
            "{bad:?}"
        );
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}
