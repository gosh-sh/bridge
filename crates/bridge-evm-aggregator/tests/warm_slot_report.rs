//! A warm `aggregate-proof` against a real pk cache: the key loads, the
//! slot it came from is reported where the caller asks, and a slot
//! published before keys recorded their length gets one. Needs a real inner
//! snark, a cache holding its slot, and the ceremony:
//!
//! ```bash
//! AGGREGATE_PROOF_E2E_SNARK=<circuit4.snark> AGGREGATE_PROOF_E2E_CACHE=<pk cache> \
//!   PARAMS_DIR=<dir with kzg_bn254_21.srs> \
//!   cargo test --release --test warm_slot_report -- --ignored --nocapture
//! ```
//!
//! The cache's `.meta.json` may be rewritten (the key's length added); its
//! `.pk` is only read, so a hard link to a real one serves.

use std::process::Command;

#[test]
#[ignore = "a real ~6.85 GB key and ~13 GB of memory: see the module doc"]
fn a_warm_run_reports_the_slot_it_loaded() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let snark = var("AGGREGATE_PROOF_E2E_SNARK");
    let cache = std::path::PathBuf::from(var("AGGREGATE_PROOF_E2E_CACHE"));
    // Without the ceremony halo2 generates an unsafe SRS of its own, into
    // this checkout; without a slot this is a full keygen into the cache.
    let params = std::path::PathBuf::from(var("PARAMS_DIR"));
    assert!(
        params.join("kzg_bn254_21.srs").is_file(),
        "PARAMS_DIR has no kzg_bn254_21.srs"
    );
    let has_slot = std::fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .any(|n| {
            n.starts_with("BridgeWithdrawalAggregatorVerifier__") && n.ends_with(".meta.json")
        });
    assert!(
        has_slot,
        "{} holds no published withdrawal slot",
        cache.display()
    );
    let dir = std::env::temp_dir().join(format!("agg_warm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let report = dir.join("aggregator-slot");
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_aggregate-proof"))
        .args(["--inner-snark", &snark])
        .args(["--name", "BridgeWithdrawalAggregatorVerifier", "--out"])
        .arg(dir.join("calldata.bin"))
        .args(["--verifiers-dir", "../../contracts/ethereum/verifiers"])
        .arg("--pk-cache-dir")
        .arg(&cache)
        .arg("--slot-report")
        .arg(&report)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("{stderr}took {:?}", started.elapsed());
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("aggregator PK cache HIT"),
        "a warm run: {stderr}"
    );
    let stem = std::fs::read_to_string(&report).expect("the slot is reported");
    let key = cache.join(format!("{stem}.pk"));
    assert!(key.is_file(), "{stem} is the slot it loaded");
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cache.join(format!("{stem}.meta.json"))).unwrap())
            .unwrap();
    assert_eq!(
        meta["pk_bytes"].as_u64(),
        Some(std::fs::metadata(&key).unwrap().len()),
        "the length is recorded"
    );
    std::fs::remove_dir_all(&dir).ok();
}
