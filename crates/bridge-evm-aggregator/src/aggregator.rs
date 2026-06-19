//! Aggregator pipeline (R15).
//!
//! Generalized wrappers over snark-verifier-sdk primitives. The keygen /
//! `break_points` dance follows the canonical pattern from
//! `snark-verifier-sdk/examples/{range_check,standard_plonk}.rs`.

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

use crate::{eip170, multiply::build_multiply_circuit};

/// Inner-circuit row count for the M2 multiply spike (`2^9` rows).
pub const K_INNER_SPIKE: u32 = 9;
pub const LOOKUP_BITS_INNER_SPIKE: usize = 8;

/// Default outer row count — safe minimum for `expose_previous_instances` @ ~13 inner PIs.
pub const K_OUTER_DEFAULT: u32 = 21;
pub const LOOKUP_BITS_OUTER_DEFAULT: usize = 20;

/// Back-compat aliases used by the M2 spike tests.
pub const K_INNER: u32 = K_INNER_SPIKE;
pub const LOOKUP_BITS_INNER: usize = LOOKUP_BITS_INNER_SPIKE;
pub const K_OUTER: u32 = K_OUTER_DEFAULT;
pub const LOOKUP_BITS_OUTER: usize = LOOKUP_BITS_OUTER_DEFAULT;

/// Tunable aggregator parameters per inner circuit.
#[derive(Debug, Clone, Copy)]
pub struct AggregatorConfig {
    pub k_outer: u32,
    pub lookup_bits_outer: usize,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            k_outer: K_OUTER_DEFAULT,
            lookup_bits_outer: LOOKUP_BITS_OUTER_DEFAULT,
        }
    }
}

impl AggregatorConfig {
    pub fn for_inner_instances(num_inner: usize) -> Self {
        // Empirics: 13 re-exposed PIs fit @ K=21 (~13 KB). Circuit 2 (14 PIs) may need K=22.
        let k_outer = if num_inner <= 13 {
            21
        } else {
            22
        };
        Self {
            k_outer,
            lookup_bits_outer: k_outer.saturating_sub(1) as usize,
        }
    }
}

/// Number of public-instance scalars contributed by the KZG accumulator (12 limbs).
pub const NUM_ACCUMULATOR_INSTANCES: usize = 12;

/// Generate a SHPLONK SNARK proving `a * b == c` (M2 spike inner circuit).
pub fn prove_inner(params: &ParamsKZG<Bn256>, a: Fr, b: Fr) -> anyhow::Result<Snark> {
    prove_inner_multiply(params, K_INNER_SPIKE, LOOKUP_BITS_INNER_SPIKE, a, b)
}

/// Generate inner multiply SNARK with explicit `k_inner`.
pub fn prove_inner_multiply(
    params: &ParamsKZG<Bn256>,
    k_inner: u32,
    lookup_bits: usize,
    a: Fr,
    b: Fr,
) -> anyhow::Result<Snark> {
    let (builder, _) = build_multiply_circuit(false, k_inner as usize, lookup_bits, a, b);
    let pk = gen_pk(params, &builder, None);
    Ok(gen_snark_shplonk(params, &pk, builder, None::<&Path>))
}

/// Wrap a pre-built inner [`Snark`] in an [`AggregationCircuit`] and prove the aggregator.
pub fn aggregate_inner(
    agg_params: &ParamsKZG<Bn256>,
    inner_snark: Snark,
    config: AggregatorConfig,
) -> anyhow::Result<Snark> {
    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };

    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        VerifierUniversality::Full,
    );
    keygen_circuit.expose_previous_instances(false);
    let calculated = keygen_circuit.calculate_params(Some(10));
    let pk = gen_pk(agg_params, &keygen_circuit, None);
    let break_points = keygen_circuit.break_points();
    drop(keygen_circuit);

    let mut prover_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calculated,
        agg_params,
        vec![inner_snark],
        VerifierUniversality::Full,
    );
    prover_circuit.expose_previous_instances(false);
    let prover_circuit = prover_circuit.use_break_points(break_points);

    Ok(gen_snark_shplonk(agg_params, &pk, prover_circuit, None::<&Path>))
}

/// Back-compat wrapper using default outer config.
pub fn aggregate(agg_params: &ParamsKZG<Bn256>, inner_snark: Snark) -> anyhow::Result<Snark> {
    aggregate_inner(agg_params, inner_snark, AggregatorConfig::default())
}

/// Generate Yul EVM verifier for an aggregator keyed on `inner_snark`.
///
/// Writes `.sol` + sibling `.bin`. When `enforce_eip170` is true, fails if bytecode
/// exceeds 24 576 bytes.
pub fn generate_yul_verifier(
    agg_params: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
    output_path: &Path,
    config: AggregatorConfig,
    enforce_eip170: bool,
) -> anyhow::Result<usize> {
    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };
    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        VerifierUniversality::Full,
    );
    keygen_circuit.expose_previous_instances(false);
    let _ = keygen_circuit.calculate_params(Some(10));
    let pk = gen_pk(agg_params, &keygen_circuit, None);
    let vk = pk.get_vk();
    let num_instance = keygen_circuit.num_instance();

    let bytecode = gen_evm_verifier_shplonk::<AggregationCircuit>(
        agg_params,
        vk,
        num_instance,
        Some(output_path),
    );

    let bin_path = output_path.with_extension("bin");
    std::fs::write(&bin_path, &bytecode)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", bin_path.display()))?;

    let size = if enforce_eip170 {
        eip170::assert_eip170(&bytecode, &output_path.display().to_string())?
    } else {
        bytecode.len()
    };
    Ok(size)
}

/// Convenience: default config + EIP-170 gate enabled.
pub fn generate_yul_verifier_gated(
    agg_params: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
    output_path: &Path,
) -> anyhow::Result<usize> {
    generate_yul_verifier(
        agg_params,
        inner_snark,
        output_path,
        AggregatorConfig::default(),
        true,
    )
}
