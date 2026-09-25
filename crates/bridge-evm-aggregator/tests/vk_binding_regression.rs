//! Regression suite locking that [`bridge_evm_aggregator::vk_binding::expose_vk_digest`]
//! actually depends on the inner circuit's preprocessed data — not just on
//! `k`, not just on the aggregator's shape, and not on the inner proof's
//! witnesses.
//!
//! # Why a separate suite is necessary
//!
//! `tests/round_trip.rs:83-89` compares the in-circuit digest against
//! [`bridge_evm_aggregator::vk_binding::expected_vk_digest`], which reuses
//! the same primitive as the in-circuit hasher. If the preimage were reduced
//! to (say) `std::iter::once(&pw.k)`, both sides would still agree on the
//! degraded value and every existing test would stay green — while the
//! on-chain adapter's `vkDigest` guard would degenerate into "any inner
//! circuit of the same `k` passes". That is the ETH-40 defect the binding
//! is meant to close, so it has to be locked by tests that compare digests
//! *across* different inner circuits of the same shape.
//!
//! The `ShplonkVkDigestPinDerivation.t.sol` Foundry test cannot help either
//! — it only checks that the pin equals word `12 + N` of the committed
//! `_calldata.bin`, which the pairing tests already enforce transitively.
//!
//! # What each test locks
//!
//! * [`digest_binds_preprocessed_full`] and its `PreprocessedAsWitness`
//!   twin — a shape-preserving inner variant (identical
//!   [`halo2_base::gates::circuit::BaseCircuitParams`], identical gate
//!   expressions, one extra copy-constraint edge in the permutation graph)
//!   must produce a *different* digest. If someone drops `pw.preprocessed`
//!   from the preimage this assertion fails, because permutation
//!   commitments live inside `preprocessed`.
//! * The same tests also lock same-circuit / different-witness *stability*:
//!   two proofs of the base circuit with different `(a, b)` inputs must
//!   produce the *same* digest. If someone accidentally hashed the proof
//!   transcript into the preimage, this assertion fails and every live
//!   proof would be rejected on chain.
//! * [`shape_preserving_variant_yields_identical_yul`] regenerates the Yul
//!   verifier for both the base and the variant and asserts they are
//!   byte-identical. That closes the loop: the on-chain Yul alone would
//!   accept the variant, so it really is the adapter's `vkDigest` gate
//!   (and not any Yul-level check) that rejects it.
//!
//! All three tests are `#[ignore]`d because the aggregator's `Keygen`-stage
//! synthesis at `K_OUTER = 21` is minutes-scale. Run locally with
//! `cargo test --release -- --ignored`.

use std::{
    env,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use bridge_evm_aggregator::{
    aggregator::{
        generate_yul_verifier, prove_inner_multiply, AggregatorConfig, K_INNER_SPIKE, K_OUTER,
        LOOKUP_BITS_INNER_SPIKE, LOOKUP_BITS_OUTER,
    },
    multiply::multiply_params,
    vk_binding::expected_vk_digest,
};
use halo2_base::{
    gates::{
        circuit::builder::BaseCircuitBuilder,
        GateChip, GateInstructions,
    },
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr},
        poly::kzg::commitment::ParamsKZG,
    },
    utils::fs::gen_srs,
};
use snark_verifier_sdk::{
    gen_pk,
    halo2::{aggregation::VerifierUniversality, gen_snark_shplonk},
    Snark,
};

/// Shape-preserving twin of
/// [`bridge_evm_aggregator::multiply::build_multiply_circuit`]. Uses the same
/// [`multiply_params`] (so column counts, gate arity, lookup arity and
/// instance count all match) and the same [`GateChip::mul`] (so the gate
/// polynomial expressions and selector patterns match). The only difference
/// is an extra witness cell copy-constrained to `c_w`, which adds one edge
/// to snark-verifier's permutation graph and so moves the preprocessed
/// permutation commitments. That is exactly the class of change the ETH-40
/// binding is required to detect.
///
/// Runtime-consistent: `c_copy` is loaded with the value of `c_w`, so the
/// copy constraint is satisfied and the proof succeeds.
fn build_multiply_circuit_variant(
    witness_gen_only: bool,
    k: usize,
    lookup_bits: usize,
    a: Fr,
    b: Fr,
) -> (BaseCircuitBuilder<Fr>, Vec<Fr>) {
    let mut builder =
        BaseCircuitBuilder::<Fr>::new(witness_gen_only).use_params(multiply_params(k, lookup_bits));

    let gate = GateChip::<Fr>::default();
    let ctx = builder.main(0);
    let a_w = ctx.load_witness(a);
    let b_w = ctx.load_witness(b);
    let c_w = gate.mul(ctx, a_w, b_w);
    let c_val = *c_w.value();

    // The extra permutation edge — shape-preserving, VK-moving.
    let c_copy = ctx.load_witness(c_val);
    ctx.constrain_equal(&c_w, &c_copy);

    builder.assigned_instances[0] = vec![c_w];

    (builder, vec![c_val])
}

fn prove_variant(params: &ParamsKZG<Bn256>, a: Fr, b: Fr) -> Snark {
    let (builder, _) = build_multiply_circuit_variant(
        /* witness_gen_only = */ false,
        K_INNER_SPIKE as usize,
        LOOKUP_BITS_INNER_SPIKE,
        a,
        b,
    );
    let pk = gen_pk(params, &builder, None);
    gen_snark_shplonk(params, &pk, builder, None::<&std::path::Path>)
}

/// Deterministic scratch directory; matches the pattern used by
/// `tests/round_trip.rs` so cached SRS files are reused across runs.
/// Resolved against the initial cwd (captured once) so that a chdir in
/// another test does not turn the relative `target` into a wrong path.
fn scratch_dir(suffix: &str) -> PathBuf {
    static INITIAL_CWD: OnceLock<PathBuf> = OnceLock::new();
    let base = INITIAL_CWD.get_or_init(|| env::current_dir().expect("initial cwd"));
    let workdir = base
        .join(env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("vk_binding_regression")
        .join(suffix);
    std::fs::create_dir_all(&workdir).expect("mkdir scratch");
    workdir
}

/// The three tests each chdir into their own scratch to keep the SRS
/// cache hermetic. `cargo test` runs test functions in parallel by
/// default, so those chdir calls race — one test's `env::current_dir()`
/// snapshot lands mid-chdir from another, and the chdir-back at the end
/// jumps to a stale path. Serialise the whole cwd-swap region under a
/// process-wide mutex.
fn chdir_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Runs three inner circuits (base × two witnesses + one shape-preserving
/// variant) and returns their aggregator VK digests under `universality`,
/// plus a repeat of the first digest to lock determinism.
fn digests_for(universality: VerifierUniversality) -> (Fr, Fr, Fr, Fr) {
    let params_inner = gen_srs(K_INNER_SPIKE);
    let params_outer = gen_srs(K_OUTER);

    let base_1 = prove_inner_multiply(
        &params_inner,
        K_INNER_SPIKE,
        LOOKUP_BITS_INNER_SPIKE,
        Fr::from(2u64),
        Fr::from(3u64),
    )
    .expect("inner (2, 3)");
    let base_2 = prove_inner_multiply(
        &params_inner,
        K_INNER_SPIKE,
        LOOKUP_BITS_INNER_SPIKE,
        Fr::from(7u64),
        Fr::from(11u64),
    )
    .expect("inner (7, 11)");
    let variant = prove_variant(&params_inner, Fr::from(2u64), Fr::from(3u64));

    let cfg = AggregatorConfig {
        k_outer: K_OUTER,
        lookup_bits_outer: LOOKUP_BITS_OUTER,
        universality,
    };

    let d_base_1 = expected_vk_digest(&params_outer, &base_1, cfg);
    let d_base_2 = expected_vk_digest(&params_outer, &base_2, cfg);
    let d_variant = expected_vk_digest(&params_outer, &variant, cfg);
    let d_base_1_repeat = expected_vk_digest(&params_outer, &base_1, cfg);

    (d_base_1, d_base_2, d_variant, d_base_1_repeat)
}

#[test]
#[ignore = "long-running (~4 min, three inner keygens + four aggregator Keygen-stage syntheses at K_OUTER=21); run with `cargo test --release -- --ignored digest_binds_preprocessed_full`"]
fn digest_binds_preprocessed_full() {
    // `gen_srs` reads cwd-relative `params/kzg_bn254_{k}.srs`; chdir into a
    // stable per-suite scratch dir so the SRS cache lives on disk across
    // runs and both keys stay hermetic. `chdir_lock` serialises the swap
    // against the other two tests in this file (see `chdir_lock` for why).
    let scratch = scratch_dir("full");
    let _guard = chdir_lock().lock().expect("chdir lock");
    let cwd = env::current_dir().expect("cwd");
    env::set_current_dir(&scratch).expect("chdir");
    let (d1, d2, dv, d1_repeat) = digests_for(VerifierUniversality::Full);
    env::set_current_dir(&cwd).expect("chdir back");
    drop(_guard);

    assert_eq!(
        d1, d1_repeat,
        "expected_vk_digest is deterministic per (agg_params, inner_snark, config)",
    );
    assert_eq!(
        d1, d2,
        "same inner circuit with different witnesses must produce the same VK digest. \
         A digest that depended on the proof would pass every existing test — because the \
         pin is only ever compared against the one calldata it was copied from — and reject \
         every live proof on chain.",
    );
    assert_ne!(
        d1, dv,
        "a shape-preserving inner variant (extra copy-constraint edge, identical \
         BaseCircuitParams, identical gate arity, identical instance count) must produce \
         a DIFFERENT digest under VerifierUniversality::Full. If this fails, the \
         `pw.preprocessed` limbs have been dropped from `expose_vk_digest`'s preimage — \
         the ETH-40 vulnerability is back: any inner circuit of the same shape passes \
         `withdrawByProof`.",
    );
}

#[test]
#[ignore = "long-running (~4 min); run with `cargo test --release -- --ignored digest_binds_preprocessed_preprocessed_as_witness`"]
fn digest_binds_preprocessed_preprocessed_as_witness() {
    let scratch = scratch_dir("preprocessed_as_witness");
    let _guard = chdir_lock().lock().expect("chdir lock");
    let cwd = env::current_dir().expect("cwd");
    env::set_current_dir(&scratch).expect("chdir");
    let (d1, d2, dv, d1_repeat) = digests_for(VerifierUniversality::PreprocessedAsWitness);
    env::set_current_dir(&cwd).expect("chdir back");
    drop(_guard);

    assert_eq!(d1, d1_repeat, "determinism under PreprocessedAsWitness");
    assert_eq!(
        d1, d2,
        "same-circuit / different-witness stability under PreprocessedAsWitness",
    );
    assert_ne!(
        d1, dv,
        "shape-preserving inner variant must produce a different digest under \
         VerifierUniversality::PreprocessedAsWitness as well (this is the setting \
         `FallbackAggregatorVerifier` uses in production)",
    );
}

#[test]
#[ignore = "long-running (~4 min, two full aggregator keygens + two Yul generations at K_OUTER=21); run with `cargo test --release -- --ignored shape_preserving_variant_yields_identical_yul`"]
fn shape_preserving_variant_yields_identical_yul() {
    let workdir = scratch_dir("yul_identity");
    let _guard = chdir_lock().lock().expect("chdir lock");
    let cwd = env::current_dir().expect("cwd");
    env::set_current_dir(&workdir).expect("chdir");

    let params_inner = gen_srs(K_INNER_SPIKE);
    let params_outer = gen_srs(K_OUTER);

    let base = prove_inner_multiply(
        &params_inner,
        K_INNER_SPIKE,
        LOOKUP_BITS_INNER_SPIKE,
        Fr::from(2u64),
        Fr::from(3u64),
    )
    .expect("inner base");
    let variant = prove_variant(&params_inner, Fr::from(2u64), Fr::from(3u64));

    env::set_current_dir(&cwd).expect("chdir back");
    drop(_guard);

    let base_yul = workdir.join("base.sol");
    let variant_yul = workdir.join("variant.sol");
    generate_yul_verifier(
        &params_outer,
        &base,
        &base_yul,
        AggregatorConfig::default(),
        /* enforce_eip170 = */ true,
    )
    .expect("Yul base");
    generate_yul_verifier(
        &params_outer,
        &variant,
        &variant_yul,
        AggregatorConfig::default(),
        true,
    )
    .expect("Yul variant");

    let base_src = std::fs::read(&base_yul).expect("read base Yul");
    let variant_src = std::fs::read(&variant_yul).expect("read variant Yul");
    assert_eq!(
        base_src, variant_src,
        "shape-preserving inner variants must yield byte-identical Yul verifiers. \
         When they do, the on-chain Yul alone cannot distinguish base from variant \
         — the adapter's `vkDigest` gate is the only thing that stops the variant's \
         aggregated proof from being accepted.",
    );
}
