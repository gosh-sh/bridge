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
//! only the 12 accumulator limbs). See this crate's README.

use std::{env, path::PathBuf};

use bridge_evm_aggregator::{
    aggregator::{
        aggregate, generate_yul_verifier_gated, prove_inner, AggregatorConfig, K_OUTER,
        NUM_ACCUMULATOR_INSTANCES,
    },
    vk_binding::{expected_vk_digest, vk_digest_index, NUM_VK_BINDING_INSTANCES},
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
        NUM_ACCUMULATOR_INSTANCES + INNER_NUM_INSTANCES + NUM_VK_BINDING_INSTANCES,
        "aggregator instance count = {} acc limbs + {} re-exposed inner PI(s) + {} VK digest(s)",
        NUM_ACCUMULATOR_INSTANCES,
        INNER_NUM_INSTANCES,
        NUM_VK_BINDING_INSTANCES,
    );
    // The re-exposed inner PI sits immediately after the accumulator limbs and
    // must equal the inner circuit's public output a*b == 77.
    assert_eq!(
        agg_snark.instances[0][NUM_ACCUMULATOR_INSTANCES],
        Fr::from(77u64),
        "re-exposed inner public input must be a*b == 77",
    );
    // VK-digest binding: the digest sits at the tail of the instance column
    // and must equal the value the native helper predicts for this inner
    // snark.
    let expected_digest =
        expected_vk_digest(&params_outer, &inner_snark, AggregatorConfig::default());
    let digest_slot = vk_digest_index(INNER_NUM_INSTANCES);
    assert_eq!(
        agg_snark.instances[0][digest_slot], expected_digest,
        "in-circuit VK digest must match the native `expected_vk_digest` value",
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

/// The digest must change when the inner constraints change, and must not
/// change when only the witness changes. The outer Yul for two same-shape
/// inners is byte-identical, so that Yul alone would accept the variant.
///
/// Run with `cargo test --release --test round_trip -- --ignored
/// digest_tracks_inner_vk`.
#[test]
#[ignore = "two inner proofs, two aggregations and two Yul builds"]
fn digest_tracks_inner_vk() {
    use bridge_evm_aggregator::aggregator::{
        generate_yul_verifier, prove_inner_plus_one, VerifierUniversality, K_INNER,
        LOOKUP_BITS_INNER,
    };

    let workdir = PathBuf::from(env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("spike-vk-digest");
    std::fs::create_dir_all(&workdir).expect("mkdir");
    let prev_cwd = env::current_dir().expect("cwd");
    env::set_current_dir(&workdir).expect("chdir");
    let params_inner = gen_srs(K_INNER);
    let params_outer = gen_srs(K_OUTER);
    env::set_current_dir(&prev_cwd).expect("chdir back");

    let mul_a = prove_inner(&params_inner, Fr::from(7u64), Fr::from(11u64)).expect("mul 7*11");
    let mul_b = prove_inner(&params_inner, Fr::from(3u64), Fr::from(5u64)).expect("mul 3*5");
    let plus = prove_inner_plus_one(
        &params_inner,
        K_INNER,
        LOOKUP_BITS_INNER,
        Fr::from(7u64),
        Fr::from(11u64),
    )
    .expect("mul+1");

    let full = AggregatorConfig::default();
    let mut as_witness = AggregatorConfig::default();
    as_witness.universality = VerifierUniversality::PreprocessedAsWitness;

    let d_full_a = expected_vk_digest(&params_outer, &mul_a, full);
    let d_full_b = expected_vk_digest(&params_outer, &mul_b, full);
    let d_full_plus = expected_vk_digest(&params_outer, &plus, full);
    assert_eq!(d_full_a, d_full_b, "witness must not change the digest");
    assert_ne!(
        d_full_a, d_full_plus,
        "a different constraint must change the digest"
    );

    let d_wit_a = expected_vk_digest(&params_outer, &mul_a, as_witness);
    let d_wit_plus = expected_vk_digest(&params_outer, &plus, as_witness);
    assert_ne!(
        d_wit_a, d_wit_plus,
        "PreprocessedAsWitness still binds the inner VK"
    );

    let yul_a = workdir.join("a.sol");
    let yul_b = workdir.join("b.sol");
    generate_yul_verifier(&params_outer, &mul_a, &yul_a, full, false).expect("yul a");
    generate_yul_verifier(&params_outer, &plus, &yul_b, full, false).expect("yul b");
    let src_a = std::fs::read(&yul_a).expect("read a");
    let src_b = std::fs::read(&yul_b).expect("read b");
    assert_eq!(
        src_a, src_b,
        "same-shape inners must regenerate byte-identical Yul"
    );

    let agg_a = aggregate(&params_outer, mul_a).expect("aggregate a");
    let agg_b = aggregate(&params_outer, mul_b).expect("aggregate b");
    let slot = vk_digest_index(1);
    assert_eq!(
        agg_a.instances[0][slot], agg_b.instances[0][slot],
        "two proofs of the same circuit must expose the same digest"
    );
    assert_eq!(agg_a.instances[0][slot], d_full_a);
}
