//! Export aggregated SHPLONK proofs + Yul verifiers for on-chain consumption.
//!
//! Calldata layout matches snark-verifier-sdk: `encode_calldata(instances, proof)`.

use std::path::Path;

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr},
        poly::kzg::commitment::ParamsKZG,
    },
};
use snark_verifier_sdk::{
    evm::{encode_calldata, gen_evm_proof_shplonk, gen_evm_verifier_shplonk},
    gen_pk,
    halo2::{
        aggregation::{AggregationCircuit, AggregationConfigParams, VerifierUniversality},
        gen_snark_shplonk,
    },
    CircuitExt, Snark, SHPLONK,
};

use crate::{
    aggregator::{
        aggregate_inner, prove_inner, AggregatorConfig, K_INNER_SPIKE, LOOKUP_BITS_INNER_SPIKE,
        NUM_ACCUMULATOR_INSTANCES,
    },
    eip170,
    multiply::build_multiply_circuit,
};

/// Full M2 spike artefacts: inner multiply proof → aggregator → Yul verifier + EVM calldata.
pub struct SpikeArtifacts {
    pub inner_a: Fr,
    pub inner_b: Fr,
    pub inner_c: Fr,
    pub agg_instances: Vec<Fr>,
    pub evm_calldata: Vec<u8>,
    pub verifier_bytecode: Vec<u8>,
    pub verifier_size: usize,
    pub k_outer: u32,
}

/// Run the multiply spike pipeline and return all exportable artefacts.
pub fn export_multiply_spike(workdir: &Path) -> anyhow::Result<SpikeArtifacts> {
    std::fs::create_dir_all(workdir)?;

    let params_inner = halo2_base::utils::fs::gen_srs(K_INNER_SPIKE);
    let config = AggregatorConfig::default();

    let a = Fr::from(7u64);
    let b = Fr::from(11u64);
    let inner_snark = prove_inner(&params_inner, a, b)?;
    let c = inner_snark.instances[0][0];

    let export = export_aggregated_snark(workdir, "MultiplierSpikeVerifier", inner_snark, config)?;
    std::fs::write(workdir.join("multiplier_spike_calldata.bin"), &export.evm_calldata)?;

    let meta = serde_json::json!({
        "inner_a": format!("{a:?}"),
        "inner_b": format!("{b:?}"),
        "inner_c": format!("{c:?}"),
        "num_accumulator_instances": NUM_ACCUMULATOR_INSTANCES,
        "total_instances": export.total_instances,
        "k_outer": export.k_outer,
        "verifier_bytes": export.verifier_size,
        "calldata_bytes": export.evm_calldata.len(),
    });
    std::fs::write(
        workdir.join("multiplier_spike_meta.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    Ok(SpikeArtifacts {
        inner_a: a,
        inner_b: b,
        inner_c: c,
        agg_instances: Vec::new(),
        evm_calldata: export.evm_calldata,
        verifier_bytecode: export.verifier_bytecode,
        verifier_size: export.verifier_size,
        k_outer: export.k_outer,
    })
}

/// Result of exporting a real inner [`Snark`] through the aggregator pipeline.
pub struct AggregatorExportResult {
    pub verifier_bytecode: Vec<u8>,
    pub verifier_size: usize,
    pub k_outer: u32,
    pub total_instances: usize,
    pub evm_calldata: Vec<u8>,
}

/// Aggregate `inner_snark`, emit Yul + `.bin` + calldata under `workdir`.
pub fn export_aggregated_snark(
    workdir: &Path,
    base_name: &str,
    inner_snark: Snark,
    config: AggregatorConfig,
) -> anyhow::Result<AggregatorExportResult> {
    std::fs::create_dir_all(workdir)?;

    let params_outer = halo2_base::utils::fs::gen_srs(config.k_outer);

    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };
    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        &params_outer,
        vec![inner_snark.clone()],
        VerifierUniversality::Full,
    );
    keygen_circuit.expose_previous_instances(false);
    let calculated = keygen_circuit.calculate_params(Some(10));
    let pk = gen_pk(&params_outer, &keygen_circuit, None);
    let break_points = keygen_circuit.break_points();
    let num_instance = keygen_circuit.num_instance();
    drop(keygen_circuit);

    let mut prover_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calculated,
        &params_outer,
        vec![inner_snark],
        VerifierUniversality::Full,
    );
    prover_circuit.expose_previous_instances(false);
    let prover_circuit = prover_circuit.use_break_points(break_points);
    let instances = prover_circuit.instances();
    let flat: Vec<Fr> = instances.iter().flat_map(|col| col.iter().copied()).collect();

    let evm_proof = gen_evm_proof_shplonk(&params_outer, &pk, prover_circuit, instances.clone());

    let sol_path = workdir.join(format!("{base_name}.sol"));
    let verifier_bytecode = gen_evm_verifier_shplonk::<AggregationCircuit>(
        &params_outer,
        pk.get_vk(),
        num_instance,
        Some(&sol_path),
    );
    let bin_path = sol_path.with_extension("bin");
    std::fs::write(&bin_path, &verifier_bytecode)?;

    let verifier_size = eip170::assert_eip170(&verifier_bytecode, &bin_path.display().to_string())?;

    let evm_calldata = encode_calldata(&instances, &evm_proof);
    std::fs::write(workdir.join(format!("{base_name}_calldata.bin")), &evm_calldata)?;

    Ok(AggregatorExportResult {
        verifier_bytecode,
        verifier_size,
        k_outer: config.k_outer,
        total_instances: flat.len(),
        evm_calldata,
    })
}

/// Deserialize an inner [`Snark`] and aggregate it with the supplied config.
pub fn aggregate_snark_from_bytes(
    params_outer: &ParamsKZG<Bn256>,
    inner_bytes: &[u8],
    config: AggregatorConfig,
) -> anyhow::Result<Snark> {
    let inner: Snark = bincode::deserialize(inner_bytes)?;
    aggregate_inner(params_outer, inner, config)
}

/// Re-export inner multiply proof generation for fixture builders.
pub fn prove_multiply_spike(
    params: &ParamsKZG<Bn256>,
    a: Fr,
    b: Fr,
) -> anyhow::Result<Snark> {
    let (builder, _) = build_multiply_circuit(
        false,
        K_INNER_SPIKE as usize,
        LOOKUP_BITS_INNER_SPIKE,
        a,
        b,
    );
    let pk = gen_pk(params, &builder, None);
    Ok(gen_snark_shplonk(params, &pk, builder, None::<&Path>))
}
