//! Export aggregated SHPLONK proofs + Yul verifiers for on-chain consumption.
//!
//! Calldata layout matches snark-verifier-sdk: `encode_calldata(instances,
//! proof)`.

use std::path::Path;

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr},
        poly::kzg::commitment::ParamsKZG,
    },
};
use snark_verifier::loader::evm::compile_solidity;
use snark_verifier_sdk::{
    evm::{encode_calldata, gen_evm_proof_shplonk},
    gen_pk,
    halo2::{aggregation::AggregationCircuit, gen_snark_shplonk},
    CircuitExt, Snark, SHPLONK,
};

use crate::{
    aggregator::{
        aggregate_inner, prove_inner, AggregatorConfig, K_INNER_SPIKE, LOOKUP_BITS_INNER_SPIKE,
        NUM_ACCUMULATOR_INSTANCES,
    },
    aggregator_cache::{keygen_or_load, CachedKeygen},
    eip170,
    multiply::build_multiply_circuit,
    srs_guard::assert_hermez_ceremony,
    verifier_source::gen_evm_verifier_sol_shplonk,
};

/// Full M2 spike artefacts: inner multiply proof → aggregator → Yul verifier +
/// EVM calldata.
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
    // Hermez guard for the inner (multiply) SRS too. The outer path is
    // separately guarded inside `aggregate_and_prove`, but keeping both
    // sides consistent avoids a spike-only trapdoor path.
    assert_hermez_ceremony(&params_inner)?;
    let config = AggregatorConfig::default();

    let a = Fr::from(7u64);
    let b = Fr::from(11u64);
    let inner_snark = prove_inner(&params_inner, a, b)?;
    let c = inner_snark.instances[0][0];

    let export = aggregate_and_prove(
        "MultiplierSpikeVerifier",
        inner_snark,
        config,
        Some(workdir),
    )?;
    let verifier_bytecode = export
        .verifier_bytecode
        .expect("an artifacts dir was given, so the verifier was compiled");
    std::fs::write(
        workdir.join("multiplier_spike_calldata.bin"),
        &export.evm_calldata,
    )?;

    let meta = serde_json::json!({
        "inner_a": format!("{a:?}"),
        "inner_b": format!("{b:?}"),
        "inner_c": format!("{c:?}"),
        "num_accumulator_instances": NUM_ACCUMULATOR_INSTANCES,
        "total_instances": export.total_instances,
        "k_outer": export.k_outer,
        "verifier_bytes": verifier_bytecode.len(),
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
        verifier_size: verifier_bytecode.len(),
        verifier_bytecode,
        k_outer: export.k_outer,
    })
}

/// Result of exporting a real inner [`Snark`] through the aggregator pipeline.
pub struct AggregatorExportResult {
    /// Generated Solidity source of the outer verifier — what `aggregate-proof`
    /// self-checks against the committed `<name>.sol`.
    pub verifier_source: String,
    /// Compiled deployment bytecode. `Some` only when an `artifacts_dir` was
    /// given: that is the regeneration path, and the only one that needs
    /// `solc`.
    pub verifier_bytecode: Option<Vec<u8>>,
    pub k_outer: u32,
    pub total_instances: usize,
    pub evm_calldata: Vec<u8>,
}

/// Aggregate `inner_snark` and produce the outer verifier bytecode + EVM
/// calldata.
///
/// Back-compat wrapper: delegates to [`aggregate_and_prove_cached`] with no PK
/// cache directory, i.e. full outer keygen on every call. Existing call sites
/// (spike, older tests) keep working unchanged. New call sites that want to
/// skip the ~3-5 min outer keygen on rerun should switch to
/// [`aggregate_and_prove_cached`].
pub fn aggregate_and_prove(
    base_name: &str,
    inner_snark: Snark,
    config: AggregatorConfig,
    artifacts_dir: Option<&Path>,
) -> anyhow::Result<AggregatorExportResult> {
    aggregate_and_prove_cached(base_name, inner_snark, config, artifacts_dir, None)
}

/// Same as [`aggregate_and_prove`] but memoises the outer keygen bundle on
/// disk when `pk_cache_dir` is `Some`. See
/// [`crate::aggregator_cache::keygen_or_load`] for slot layout and cache-key
/// invariants.
///
/// `pk_cache_dir` is orthogonal to `artifacts_dir`:
/// - `artifacts_dir` decides whether the verifier is compiled at all. With a
///   directory, `<name>.sol` and `<name>.bin` are written there and the
///   bytecode is gated on EIP-170 — the verifier-generation path
///   (`export-inner-aggregator`), which needs `solc 0.8.19` on `PATH`. Without
///   one only the source is generated, which is all the runtime self-check in
///   `aggregate-proof` compares, and no compiler is involved.
/// - `pk_cache_dir` controls whether the outer PK is persisted for reuse across
///   runs (runtime aggregation path, `aggregate-proof`).
pub fn aggregate_and_prove_cached(
    base_name: &str,
    inner_snark: Snark,
    config: AggregatorConfig,
    artifacts_dir: Option<&Path>,
    pk_cache_dir: Option<&Path>,
) -> anyhow::Result<AggregatorExportResult> {
    let params_outer = halo2_base::utils::fs::gen_srs(config.k_outer);
    // Refuse to run against toxic-waste SRS: `gen_srs` silently generates
    // an unsafe SRS when PARAMS_DIR is missing kzg_bn254_{k}.srs. Same
    // Hermez PPoT anchor used by bridge-prover-lib.
    assert_hermez_ceremony(&params_outer)?;

    let CachedKeygen {
        pk,
        break_points,
        calculated,
        num_instance,
    } = keygen_or_load(&params_outer, base_name, config, &inner_snark, pk_cache_dir)?;

    let mut prover_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calculated,
        &params_outer,
        vec![inner_snark],
        config.universality,
    );
    prover_circuit.expose_previous_instances(false);
    // Append the VK digest so the prover's instance count matches the outer
    // VK (which `keygen_or_load` built with the digest slot included). The
    // outer PK enforces `num_instance == 12 + N + 1`, so omitting this step
    // would fail the prover's instance-shape check before any proof leaves
    // this process.
    crate::vk_binding::expose_vk_digest(&mut prover_circuit);
    let prover_circuit = prover_circuit.use_break_points(break_points);
    let instances = prover_circuit.instances();
    let flat: Vec<Fr> = instances
        .iter()
        .flat_map(|col| col.iter().copied())
        .collect();

    // DIAG: dump instance layout so we can confirm what actually lives at each
    // slot (esp. positions 12..NUM_ACCUMULATOR_INSTANCES+N_inner). When
    // BRIDGE_DIAG_INSTANCES_FILE is set, appends `base_name`, column count,
    // and every Fr in column 0 as BE hex (matches Solidity `_readInstance`).
    // Env var (not env=1) because the daemon's SubprocessAggregator swallows
    // subprocess stderr on success.
    if let Ok(diag_path) = std::env::var("BRIDGE_DIAG_INSTANCES_FILE") {
        use std::io::Write;
        let mut buf = String::new();
        buf.push_str(&format!(
            "BRIDGE_DIAG_INSTANCES base_name={base_name} num_instance_cols={} col0_len={}\n",
            instances.len(),
            instances.first().map(|c| c.len()).unwrap_or(0),
        ));
        if let Some(col0) = instances.first() {
            for (i, fr) in col0.iter().enumerate() {
                let bytes = bincode::serialize(fr).unwrap_or_default();
                let mut le = [0u8; 32];
                let n = bytes.len().min(32);
                le[..n].copy_from_slice(&bytes[..n]);
                let mut be = le;
                be.reverse();
                buf.push_str(&format!(
                    "  inst[{i:>2}] BE=0x{}\n",
                    be.iter().map(|b| format!("{:02x}", b)).collect::<String>()
                ));
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&diag_path)
        {
            let _ = f.write_all(buf.as_bytes());
        }
    }

    let evm_proof = gen_evm_proof_shplonk(&params_outer, &pk, prover_circuit, instances.clone());

    let verifier_source = gen_evm_verifier_sol_shplonk::<AggregationCircuit>(
        &params_outer,
        pk.get_vk(),
        num_instance,
    );
    let verifier_bytecode = match artifacts_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            // Compile before writing anything: `compile_solidity` shells out
            // to `solc` and panics if it is missing, and this path is the
            // only one that still needs it. Writing `.sol` first would leave
            // a half pair — a fresh source next to a stale `.bin` — for the
            // CI check that exists to catch exactly that.
            let bytecode = compile_solidity(&verifier_source);
            std::fs::write(dir.join(format!("{base_name}.sol")), &verifier_source)?;
            let bin_path = dir.join(format!("{base_name}.bin"));
            std::fs::write(&bin_path, &bytecode)?;
            // After the write, as before: an oversized verifier stays on disk
            // for inspection.
            eip170::assert_eip170(&bytecode, &bin_path.display().to_string())?;
            Some(bytecode)
        },
        None => None,
    };

    let evm_calldata = encode_calldata(&instances, &evm_proof);

    Ok(AggregatorExportResult {
        verifier_source,
        verifier_bytecode,
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
pub fn prove_multiply_spike(params: &ParamsKZG<Bn256>, a: Fr, b: Fr) -> anyhow::Result<Snark> {
    let (builder, _) =
        build_multiply_circuit(false, K_INNER_SPIKE as usize, LOOKUP_BITS_INNER_SPIKE, a, b);
    let pk = gen_pk(params, &builder, None);
    Ok(gen_snark_shplonk(params, &pk, builder, None::<&Path>))
}
