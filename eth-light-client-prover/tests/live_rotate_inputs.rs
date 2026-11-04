//! Live rotate inputs: an update file that is named but cannot be used is an
//! error, never a fixture or a synthetic committee.

use std::path::PathBuf;

use eth_light_client_prover::committee::{COMMITTEE_SHARDS, PUBKEYS_PER_SHARD};
use eth_light_client_prover::mainnet::{committee_source_at, SLOTS_PER_SYNC_PERIOD};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-live-inputs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A `/updates?start_period=P&count=1` answer: an array of one update whose
/// committee has 512 distinct pubkeys.
fn update_json(period: u64) -> String {
    let pubkeys: Vec<String> = (0..512u32)
        .map(|i| format!("\"0x{}{:08x}\"", "a".repeat(88), i))
        .collect();
    let branch: Vec<String> = (0..6).map(|i| format!("\"0x{}\"", format!("{i:02x}").repeat(32))).collect();
    format!(
        r#"[{{"version":"fulu","data":{{
            "attested_header":{{"beacon":{{"slot":"{slot}","state_root":"0x{root}"}}}},
            "next_sync_committee":{{"pubkeys":[{pubkeys}],"aggregate_pubkey":"0x{agg}"}},
            "next_sync_committee_branch":[{branch}],
            "signature_slot":"{sig}"
        }}}}]"#,
        slot = period * SLOTS_PER_SYNC_PERIOD + 100,
        sig = period * SLOTS_PER_SYNC_PERIOD + 101,
        root = "11".repeat(32),
        agg = "bb".repeat(48),
        pubkeys = pubkeys.join(","),
        branch = branch.join(","),
    )
}

#[test]
fn a_live_update_gives_its_committee_in_eight_shards() {
    let dir = scratch("ok");
    let path = dir.join("update.json");
    std::fs::write(&path, update_json(1200)).unwrap();
    let source = committee_source_at(&path).unwrap();
    assert_eq!(source.shards.len(), COMMITTEE_SHARDS);
    assert!(source.shards.iter().all(|s| s.len() == PUBKEYS_PER_SHARD));
    assert_eq!(source.shards[7][63][44..48], 511u32.to_be_bytes());
    let update = source.update.expect("the live update");
    assert_eq!(update.period(), 1201);
    assert!(source.label.starts_with("live "), "{}", source.label);
}

#[test]
fn a_named_update_that_cannot_be_used_is_an_error() {
    let dir = scratch("bad");
    let missing = dir.join("missing.json");
    let err = committee_source_at(&missing).unwrap_err().to_string();
    assert!(err.contains("ROTATE_UPDATE_PATH") && err.contains("missing.json"), "{err}");

    let corrupt = dir.join("corrupt.json");
    std::fs::write(&corrupt, "{ not json").unwrap();
    assert!(committee_source_at(&corrupt).is_err());

    // What the beacon node answers before the period has an update.
    let empty = dir.join("empty.json");
    std::fs::write(&empty, "[]").unwrap();
    assert!(committee_source_at(&empty).is_err());

    let short = dir.join("short.json");
    std::fs::write(&short, update_json(1200).replacen("\"0xaaaa", "\"0xaa", 1)).unwrap();
    assert!(committee_source_at(&short).is_err());
}
