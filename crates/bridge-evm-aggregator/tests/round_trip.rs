//! M2 spike — end-to-end round-trip acceptance check.
//!
//! Runs the full pipeline from the trivial multiply circuit through the
//! aggregator to a Yul EVM verifier source file and verifies:
//!
//! 1. The inner SHPLONK proof can be aggregated.
//! 2. The aggregator's emitted Yul source is <= 24 576 bytes (EIP-170).
//! 3. The aggregator re-exposes the inner public input: the instance layout is
//!    `[acc_0 .. acc_11, c]` — 12 KZG accumulator limbs followed by the inner
//!    circuit's `c = a * b`.
//!
//! Item 3 is the M5 advance over the original M2 acceptance (which exposed
//! only the 12 accumulator limbs). See `docs/r15_snark_verifier_roadmap.md`.

use std::{env, path::PathBuf};

use bridge_evm_aggregator::aggregator::{
    aggregate, generate_yul_verifier_gated, prove_inner, K_OUTER, NUM_ACCUMULATOR_INSTANCES,
};
use halo2_base::{halo2_proofs::halo2curves::bn256::Fr, utils::fs::gen_srs};

/// End-to-end pipeline. Skipped by default on CI (long: ~3 min on a laptop);
/// run with `cargo test --release -- --ignored aggregator_round_trip`.
#[test]
#[ignore = "long-running (~3 min); run with --ignored locally for M2 acceptance"]
fn aggregator_round_trip() {
    // Persist working state under target/spike/ so reruns of the test stay
    // hermetic but keep the SRS caches across runs.
    let workdir = PathBuf::from(env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("spike");
    std::fs::create_dir_all(&workdir).expect("mkdir target/spike");
    // gen_srs reads cwd-relative `params/kzg_bn254_{k}.srs`; jump there so
    // both K_INNER and K_OUTER SRS files (if pre-supplied) are discovered.
    let prev_cwd = env::current_dir().expect("cwd");
    env::set_current_dir(&workdir).expect("chdir target/spike");

    // SRS — fall back to halo2-base's deterministic test SRS if no cached
    // file is present (the spike is not production; M5 will pin production
    // SRS).
    let params_inner = gen_srs(bridge_evm_aggregator::aggregator::K_INNER);
    let params_outer = gen_srs(K_OUTER);

    let a = Fr::from(7u64);
    let b = Fr::from(11u64);
    let inner_snark = prove_inner(&params_inner, a, b).expect("prove inner");
    assert_eq!(
        inner_snark.instances,
        vec![vec![Fr::from(77u64)]],
        "inner instance must be a*b == 77"
    );

    // The multiply inner circuit exposes exactly one public input (`c`).
    const INNER_NUM_INSTANCES: usize = 1;

    let agg_snark = aggregate(&params_outer, inner_snark.clone()).expect("aggregate");
    assert_eq!(
        agg_snark.instances.len(),
        1,
        "aggregator exposes a single instance column",
    );
    assert_eq!(
        agg_snark.instances[0].len(),
        NUM_ACCUMULATOR_INSTANCES + INNER_NUM_INSTANCES,
        "aggregator instance count = {} acc limbs + {} re-exposed inner PI(s)",
        NUM_ACCUMULATOR_INSTANCES,
        INNER_NUM_INSTANCES,
    );
    // The re-exposed inner PI sits immediately after the accumulator limbs and
    // must equal the inner circuit's public output a*b == 77.
    assert_eq!(
        agg_snark.instances[0][NUM_ACCUMULATOR_INSTANCES],
        Fr::from(77u64),
        "re-exposed inner public input must be a*b == 77",
    );

    let yul_path = workdir.join("AggregatorVerifierSpike.sol");
    env::set_current_dir(&prev_cwd).expect("chdir back");
    let bytecode_size = generate_yul_verifier_gated(&params_outer, &inner_snark, &yul_path)
        .expect("generate Yul verifier");

    println!(
        "[M2 spike] Yul verifier bytecode size: {} bytes ({:.1} KB)",
        bytecode_size,
        bytecode_size as f64 / 1024.0,
    );
    assert!(
        bytecode_size <= 24_576,
        "Yul verifier exceeds EIP-170 24 576 byte limit: {} bytes",
        bytecode_size,
    );

    let yul_src = std::fs::read_to_string(&yul_path).expect("read Yul");
    // snark-verifier-sdk emits `contract Halo2Verifier { fallback(bytes calldata)
    // external returns (bytes memory) }` — the calldata is `instances ‖ proof`,
    // success iff the fallback returns non-zero. M6 will wrap this in
    // `BridgeWithdrawalAggregatorVerifier.sol` with a `verifyProof` adapter.
    assert!(
        yul_src.contains("contract Halo2Verifier"),
        "expected Yul to declare a `contract Halo2Verifier`, source head:\n{}",
        &yul_src[..yul_src.len().min(400)]
    );
    assert!(
        yul_src.contains("fallback(bytes calldata)"),
        "expected Yul to expose `fallback(bytes calldata)`, source head:\n{}",
        &yul_src[..yul_src.len().min(400)]
    );
    println!("[M2 spike] Yul source: {} bytes", yul_src.len());
}
