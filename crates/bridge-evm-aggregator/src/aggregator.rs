//! Aggregator pipeline (M2 spike).
//!
//! Three thin wrappers over snark-verifier-sdk primitives. The keygen /
//! `break_points` dance follows the canonical pattern from
//! `snark-verifier-sdk/examples/{range_check,standard_plonk}.rs`:
//!
//! - **inner**: build one `BaseCircuitBuilder` with `witness_gen_only=false`,
//!   pass it to `gen_pk` (which populates break-points internally as a side
//!   effect of running keygen), then pass the **same** builder to
//!   `gen_snark_shplonk`.
//! - **aggregator**: build the `AggregationCircuit` in `Keygen` stage, call
//!   `calculate_params`, then `gen_pk`, then read `break_points` (must be AFTER
//!   `gen_pk` — `calculate_params` alone does not populate them), then drop the
//!   keygen circuit and build a fresh `Prover`-stage circuit with
//!   `.use_break_points(break_points)`.

use std::path::Path;

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr},
        poly::kzg::commitment::ParamsKZG,
    },
};
use snark_verifier_sdk::{
    evm::gen_evm_verifier_shplonk,
    gen_pk,
    halo2::{
        aggregation::{AggregationCircuit, AggregationConfigParams, VerifierUniversality},
        gen_snark_shplonk,
    },
    CircuitExt, Snark, SHPLONK,
};

use crate::multiply::build_multiply_circuit;

/// Inner-circuit row count (binary log). `2^9 = 512` rows easily fits a
/// single multiplication gate; we keep `k_inner` small so SRS load + keygen
/// take seconds, not minutes.
pub const K_INNER: u32 = 9;
/// Inner-circuit lookup-bits. The multiply gate doesn't actually use the
/// range lookup, but [`AggregationCircuit`] requires the inner circuit's
/// lookup chip to be initialised, so we keep `lookup_bits = k - 1`.
pub const LOOKUP_BITS_INNER: usize = 8;

/// Outer (aggregator) row count. snark-verifier-sdk's `standard_plonk.rs`
/// example uses `k = 21` with `expose_previous_instances`; `k = 20` overflows
/// the advice columns by a handful of rows once we re-expose inner inputs
/// (NOT ENOUGH ADVICE COLUMNS @ runtime). We may revisit when sizing the
/// real Circuit 4 aggregator (M5) — for now `k = 21` is the safe minimum.
pub const K_OUTER: u32 = 21;
/// Aggregator lookup-bits. Must be `< k_outer`. snark-verifier-sdk's default
/// is `k - 1` for the deposit-prover empirics — same here.
pub const LOOKUP_BITS_OUTER: usize = 20;

/// Generate a SHPLONK SNARK proving `a * b == c` for the supplied scalars.
///
/// Uses snark-verifier-sdk's default transcript (Poseidon — the only choice
/// compatible with `AggregationCircuit` without writing a custom in-circuit
/// transcript loader).
pub fn prove_inner(params: &ParamsKZG<Bn256>, a: Fr, b: Fr) -> anyhow::Result<Snark> {
    let (builder, _) = build_multiply_circuit(
        // witness_gen_only =
        false,
        K_INNER as usize,
        LOOKUP_BITS_INNER,
        a,
        b,
    );

    // `gen_pk` runs keygen on `builder` and, as a side effect, populates the
    // builder's break-points. `gen_snark_shplonk` on the SAME builder then
    // reuses them. This matches `range_check.rs:41-51` upstream.
    let pk = gen_pk(params, &builder, None);
    let snark = gen_snark_shplonk(params, &pk, builder, None::<&Path>);
    Ok(snark)
}

/// Number of public-instance scalars contributed by the KZG accumulator.
///
/// snark-verifier-sdk represents the accumulator as **two G1 points**
/// (`lhs`, `rhs` of the pairing check), each stored as **two non-native
/// field elements** (x and y in `Fq` for BN254), and each non-native element
/// is decomposed into `LIMBS = 3` 88-bit native limbs. So the total is
/// `2 * 2 * 3 = 12` scalars. This matches the runtime assertion in the M2
/// round-trip test.
///
/// All four `*Verifier.sol` adapters generated downstream must allocate at
/// least these 12 slots BEFORE any inner circuit's public inputs.
pub const NUM_ACCUMULATOR_INSTANCES: usize = 12;

/// Wrap `inner_snark` in an [`AggregationCircuit`] and prove the aggregator.
///
/// `agg_params` is the outer SRS (`k = K_OUTER`). The returned [`Snark`]
/// has instance layout `[acc_0 .. acc_11]` — exactly
/// [`NUM_ACCUMULATOR_INSTANCES`] = 12 limbs of the KZG pairing accumulator.
/// The inner circuit's public inputs are intentionally **not** re-exposed
/// at this stage of the spike (the bare `expose_previous_instances(false)`
/// upstream call interacts poorly with the auto-tuned advice column count
/// — `NOT ENOUGH ADVICE COLUMNS` at K=20 / 21; needs custom
/// `AggregationConfigParams.num_advice` to fit deterministically).
/// Re-exposing them is the first real-world task for **M5** when we wire
/// the partner's Circuit 4 in place of `multiply`; the bridge contract
/// reads the 10 PIs (`tokenId`, `amount`, …, `finalRoot`) **after** that
/// step lands.
///
/// `Universality::Full` means the verifying key of the inner SNARK is
/// loaded as a witness — same aggregator PK can verify proofs from
/// different inner circuits / BK-set rotations without keygen.
pub fn aggregate(agg_params: &ParamsKZG<Bn256>, inner_snark: Snark) -> anyhow::Result<Snark> {
    let agg_config = AggregationConfigParams {
        degree: K_OUTER,
        lookup_bits: LOOKUP_BITS_OUTER,
        ..Default::default()
    };

    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        VerifierUniversality::Full,
    );
    let calculated = keygen_circuit.calculate_params(Some(10));
    // ORDER MATTERS: gen_pk first (populates break-points), break_points after.
    let pk = gen_pk(agg_params, &keygen_circuit, None);
    let break_points = keygen_circuit.break_points();
    drop(keygen_circuit);

    let prover_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calculated,
        agg_params,
        vec![inner_snark],
        VerifierUniversality::Full,
    )
    .use_break_points(break_points);

    let agg_snark = gen_snark_shplonk(agg_params, &pk, prover_circuit, None::<&Path>);
    Ok(agg_snark)
}

/// Generate the Yul EVM verifier for an [`AggregationCircuit`] proved
/// against `agg_params`.
///
/// Writes the human-readable Solidity source to `output_path` (`.sol`) and
/// the raw deployment bytecode (what `gen_evm_verifier_shplonk` returns)
/// to a sibling `.bin` file with the same stem. The bytecode is the
/// **deployable** payload — the Solidity source is provided for review
/// only; Foundry's solc + optimizer strips the inline-assembly fallback
/// down to a 67-byte stub (this is why the legacy
/// `test/halo2_verifier_bytecode.bin` exists; we follow the same
/// convention).
///
/// Returns the deployment bytecode size in bytes (the EIP-170 24 576-byte
/// runtime limit guard belongs in the caller).
///
/// `inner_snark` is used only to seed the aggregator's keygen — it is not
/// consumed by the verifier. In production we'd cache the aggregator's
/// VK on disk just like `deposit-prover/src/aggregation.rs` does, but for
/// the spike a fresh keygen each call keeps the flow obvious.
pub fn generate_yul_verifier(
    agg_params: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
    output_path: &Path,
) -> anyhow::Result<usize> {
    let agg_config = AggregationConfigParams {
        degree: K_OUTER,
        lookup_bits: LOOKUP_BITS_OUTER,
        ..Default::default()
    };
    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        VerifierUniversality::Full,
    );
    let _ = keygen_circuit.calculate_params(Some(10));
    let pk = gen_pk(agg_params, &keygen_circuit, None);
    let vk = pk.get_vk();

    // Default aggregator instance shape — see `aggregate` doc for why we
    // are NOT calling `expose_previous_instances` at this stage.
    let num_instance = vec![NUM_ACCUMULATOR_INSTANCES];

    let bytecode = gen_evm_verifier_shplonk::<AggregationCircuit>(
        agg_params,
        vk,
        num_instance,
        Some(output_path),
    );

    // Persist raw deployable bytecode next to the .sol source so the Foundry
    // harness can `vm.readFileBinary` + create2 it without going through
    // solc (which strips the inline-assembly verifier — see doc above).
    let bin_path = output_path.with_extension("bin");
    std::fs::write(&bin_path, &bytecode)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", bin_path.display()))?;

    Ok(bytecode.len())
}

// Suppress unused-import warning on CircuitExt — it is brought into scope so
// downstream callers can call `.instances()` and `.num_instance()` on the
// AggregationCircuit values they construct.
#[allow(dead_code)]
fn _circuit_ext_in_scope<T: CircuitExt<Fr>>(_: &T) {}
