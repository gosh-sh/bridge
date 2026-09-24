//! Bind the inner-circuit verification key inside the aggregator.
//!
//! # Why this exists
//!
//! `AggregationCircuit` is built with `VerifierUniversality::Full` (or
//! `PreprocessedAsWitness`) for every production aggregator in this repo. Under
//! those settings, the inner snark's verifying-key witnesses (preprocessed
//! commitments, `transcript_initial_state`, and — under `Full` — `k`) are
//! loaded as **witnesses** the prover supplies. Nothing in the aggregator or
//! the on-chain Yul verifier compares those witnesses against a known value,
//! so the on-chain check only pins the *shape* of the inner circuit
//! (# columns, # instances, gate expressions, lookup arity). Two circuits
//! with the same shape but different constraints produce different VKs — and
//! the aggregator accepts a proof from either.
//!
//! In practice this means an attacker who takes the public Circuit 4 source,
//! deletes copy-constraints that bind Merkle path ↔ nullifier ↔ `finalRoot`,
//! and keeps the same gate/selector layout produces a shape-preserving
//! impostor whose aggregated proof passes `withdrawByProof`. Same defect
//! affects `verifyBlock` and `applyBkSetUpdate`.
//!
//! # The fix
//!
//! Hash the inner-VK witnesses inside the aggregator and expose the digest
//! as an additional public instance (the last element of instance column 0,
//! placed after the KZG accumulator and any re-exposed inner instances).
//! On-chain adapters carry an `immutable bytes32 vkDigest` set at deploy
//! time from [`expected_vk_digest`] and reject proofs whose exposed digest
//! doesn't match.
//!
//! Compared to `VerifierUniversality::None` (bake VK as circuit constants),
//! this keeps the outer Yul verifier universal by shape. A same-shape
//! rotation of the inner circuit (identical column counts and gate/lookup
//! arity, different constraints) still requires an `AckiNackiBridge`
//! redeploy — the four verifier slots are `immutable` — but the deployed
//! Yul does not have to be regenerated: only the adapter carries a fresh
//! `vkDigest`. Under `VerifierUniversality::PreprocessedAsWitness` (the
//! Fallback adapter) a change of inner `k` also changes the Yul, so that
//! case still needs a full aggregator regeneration.
//!
//! # Hash choice
//!
//! Poseidon over BN254 with the exact parameters snark-verifier-sdk uses for
//! its transcript (T=3, RATE=2, R_F=8, R_P=57, `SECURE_MDS=0` — Scroll's
//! p128pow5t3 spec). Reusing the same spec avoids introducing a second
//! Poseidon parameter set into the aggregator's proving key.

use halo2_base::{
    gates::{circuit::CircuitBuilderStage, RangeInstructions},
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr},
        poly::kzg::commitment::ParamsKZG,
    },
    poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher},
    AssignedValue,
};
use snark_verifier_sdk::{
    halo2::aggregation::{AggregationCircuit, AggregationConfigParams},
    Snark, SHPLONK,
};

use crate::aggregator::{AggregatorConfig, NUM_ACCUMULATOR_INSTANCES};

/// Poseidon state width. Must match `snark-verifier-sdk::halo2::T`.
pub const VK_DIGEST_T: usize = 3;
/// Poseidon absorb rate. Must match `snark-verifier-sdk::halo2::RATE`.
pub const VK_DIGEST_RATE: usize = 2;
/// Full-round count. Must match `snark-verifier-sdk::halo2::R_F`.
const VK_DIGEST_R_F: usize = 8;
/// Partial-round count. Must match `snark-verifier-sdk::halo2::R_P`.
const VK_DIGEST_R_P: usize = 57;
/// Secure-MDS index. `0` matches snark-verifier-sdk's transcript spec (Scroll
/// p128pow5t3).
const VK_DIGEST_SECURE_MDS: usize = 0;

/// Number of public instances the aggregator emits in addition to the KZG
/// accumulator and any re-exposed inner instances. Currently one (the VK
/// digest).
pub const NUM_VK_BINDING_INSTANCES: usize = 1;

/// Append a Poseidon digest of the inner-snark VK witnesses to the aggregator's
/// public-instance column. Must be called **after** `expose_previous_instances`
/// so that the layout is:
///
/// ```text
///   [ accumulator (12) | previous_instances (N) | vk_digest (1) ]
/// ```
///
/// Placing the digest last means existing on-chain adapter code that reads
/// PIs at indices `12..12+N` does not shift; adapters only add one new
/// `_readInstance(proof, 12 + N)` check.
///
/// Digest preimage per inner snark:
///   * `preprocessed_witnesses.preprocessed` — one Fr per limb of every
///     preprocessed commitment the aggregator loads (fixed / selector /
///     permutation columns). The exact count is the inner circuit's
///     preprocessed-column count × 2 (x, y) × non-native-field limbs, plus
///     the `transcript_initial_state` element that snark-verifier appends
///     — always present on this path, not optional. Circuit 4 (11 inner
///     PIs, ~19 preprocessed commitments at K=19) and Circuit 1A/1B are
///     both larger than a hundred elements.
///   * `preprocessed_witnesses.k` — witness under `Full`, loaded constant
///     otherwise (uniform code path either way).
pub fn expose_vk_digest(agg: &mut AggregationCircuit) {
    // Flatten every inner snark's VK witnesses first — this borrows `agg`
    // immutably. The aggregator currently aggregates exactly one inner snark,
    // but this generalises cleanly.
    let inputs: Vec<AssignedValue<Fr>> = agg
        .preprocessed()
        .iter()
        .flat_map(|pw| pw.preprocessed.iter().chain(std::iter::once(&pw.k)).copied())
        .collect();
    assert!(
        !inputs.is_empty(),
        "aggregator has no inner-snark preprocessed witnesses to bind",
    );

    let range = agg.builder.range_chip();
    let gate = range.gate().clone();
    let ctx = agg.builder.main(0);

    let spec = OptimizedPoseidonSpec::<Fr, VK_DIGEST_T, VK_DIGEST_RATE>::new::<
        VK_DIGEST_R_F,
        VK_DIGEST_R_P,
        VK_DIGEST_SECURE_MDS,
    >();
    let mut hasher = PoseidonHasher::<Fr, VK_DIGEST_T, VK_DIGEST_RATE>::new(spec);
    hasher.initialize_consts(ctx, &gate);

    let digest = hasher.hash_fix_len_array(ctx, &gate, &inputs);
    agg.builder.assigned_instances[0].push(digest);
}

/// Compute the exact VK-digest value the aggregator will emit for a given
/// inner snark, without generating a proving key or a proof.
///
/// Deterministic in `inner_snark.protocol` (the inner VK — the preprocessed
/// commitments and `transcript_initial_state`), `agg_params` (via the SRS
/// `k` under `Full`), and `config.universality`. Independent of
/// `config.k_outer` / `lookup_bits_outer` — those only shape the outer
/// circuit and do not enter the digest preimage.
///
/// # Cost
///
/// Runs a keygen-stage synthesis of the aggregator circuit: builds the
/// aggregation circuit, calls `expose_previous_instances` and
/// `expose_vk_digest`, then reads `assigned_instances[0].last().value()`.
/// No `calculate_params`, no FFTs, no PK generation, no proof — cost is
/// dominated by the aggregation-circuit synthesis itself.
pub fn expected_vk_digest(
    agg_params: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
    config: AggregatorConfig,
) -> Fr {
    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };
    let mut circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        config.universality,
    );
    // Layout parity with the runtime prover path: expose previous instances
    // first (matches `aggregate_inner_cached`), then append the digest so
    // it lands at the same slot the on-chain adapter reads.
    circuit.expose_previous_instances(false);
    expose_vk_digest(&mut circuit);

    *circuit
        .builder
        .assigned_instances[0]
        .last()
        .expect("expose_vk_digest pushes at least one instance")
        .value()
}

/// Index (within the aggregator's single public-instance column) at which
/// on-chain adapters read the VK digest.
///
/// Layout: `[accumulator (12) | previous_instances (num_prev) | vk_digest (1)]`,
/// so the digest sits at `NUM_ACCUMULATOR_INSTANCES + num_prev`. Adapters
/// know `num_prev` at compile time (= their `NUM_INNER`). Consumed by the
/// round-trip test and re-exportable for any future in-workspace caller
/// that needs to reason about the layout; downstream crates that cannot
/// pull `bridge-evm-aggregator` (halo2 backend clash — see the workspace
/// note in `AGENTS.md`) must replicate this arithmetic and are expected
/// to stay in step with the constant here.
pub const fn vk_digest_index(num_prev_instances: usize) -> usize {
    NUM_ACCUMULATOR_INSTANCES + num_prev_instances
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the layout the on-chain adapters rely on: accumulator first
    /// (12 limbs), then the re-exposed inner PIs, then exactly one VK digest
    /// at the tail. Adapters read the digest at [`vk_digest_index`] and
    /// verify the outer proof carries `NUM_ACCUMULATOR_INSTANCES +
    /// num_prev + NUM_VK_BINDING_INSTANCES` scalars in column 0.
    #[test]
    fn digest_lands_after_accumulator_and_previous_instances() {
        // Circuit 4: 11 inner PIs (see BridgeWithdrawalAggregatorVerifier).
        assert_eq!(vk_digest_index(11), 23);
        // Circuit 2: 14 inner PIs.
        assert_eq!(vk_digest_index(14), 26);
        // Boundary: no inner PIs re-exposed.
        assert_eq!(vk_digest_index(0), NUM_ACCUMULATOR_INSTANCES);
    }

    /// Poseidon parameters must match snark-verifier-sdk's transcript spec
    /// verbatim. If the sdk ever bumps its constants, the in-circuit hasher
    /// here would silently diverge from the transcript Poseidon — likely
    /// harmless (both are independent uses) but confusing and worth
    /// catching.
    ///
    /// The sdk keeps its `T`/`RATE`/`R_F`/`R_P`/`SECURE_MDS` private, but
    /// exports `POSEIDON_SPEC` as a public `OptimizedPoseidonSpec<Fr, T,
    /// RATE>`. We pin `T` and `RATE` at the type level via an explicit
    /// annotation, and cross-check `R_F` (via `.r_f()`) and `R_P` (via the
    /// length of the partial-round constants) against the live sdk spec —
    /// so a future sdk bump of any of those fails this test.
    #[test]
    fn poseidon_spec_matches_snark_verifier_sdk_transcript() {
        use snark_verifier_sdk::halo2::POSEIDON_SPEC;

        // Type-level pin: fails to compile if the sdk changes T or RATE.
        let sdk: &OptimizedPoseidonSpec<Fr, VK_DIGEST_T, VK_DIGEST_RATE> = &POSEIDON_SPEC;

        assert_eq!(sdk.r_f(), VK_DIGEST_R_F, "R_F drifted from snark-verifier-sdk");
        assert_eq!(
            sdk.constants().partial().len(),
            VK_DIGEST_R_P,
            "R_P drifted from snark-verifier-sdk"
        );

        // Local sanity pins for the literals so the test also documents
        // intent independent of the sdk cross-check.
        assert_eq!(VK_DIGEST_T, 3);
        assert_eq!(VK_DIGEST_RATE, 2);
        assert_eq!(VK_DIGEST_R_F, 8);
        assert_eq!(VK_DIGEST_R_P, 57);
        assert_eq!(VK_DIGEST_SECURE_MDS, 0);
    }
}
