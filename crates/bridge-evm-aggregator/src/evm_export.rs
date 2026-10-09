//! Export aggregated SHPLONK proofs + Yul verifiers for on-chain consumption.
//!
//! Calldata layout matches snark-verifier-sdk: `encode_calldata(instances,
//! proof)`.

use std::path::{Path, PathBuf};

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
    aggregator_cache::{keygen_or_load, CachedKeygen, KeySource},
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
    /// The stem of the pk-cache slot the outer key came from or went to;
    /// `None` without a cache.
    pub pk_cache_slot: Option<String>,
}

/// Aggregate `inner_snark` and produce the outer verifier bytecode + EVM
/// calldata.
///
/// Back-compat wrapper: delegates to [`aggregate_and_prove_cached`] with no PK
/// cache directory, i.e. full outer keygen on every call. Existing call sites
/// (spike, older tests) keep working unchanged. New call sites that want to
/// skip the outer keygen on rerun should switch to
/// [`aggregate_and_prove_cached`].
pub fn aggregate_and_prove(
    base_name: &str,
    inner_snark: Snark,
    config: AggregatorConfig,
    artifacts_dir: Option<&Path>,
) -> anyhow::Result<AggregatorExportResult> {
    aggregate_and_prove_cached(base_name, inner_snark, config, artifacts_dir, None)
}

/// Run `prove`, and turn a panic in it into `explain` of its message.
fn proof_or<T>(
    prove: impl FnOnce() -> T,
    explain: impl FnOnce(&str) -> anyhow::Error,
) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(prove)).map_err(|panic| {
        let text = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string());
        explain(&text)
    })
}

/// Where `gen_srs(k)` reads the ceremony from (halo2-base's
/// `read_or_create_srs`).
fn srs_path(k: u32) -> PathBuf {
    PathBuf::from(std::env::var("PARAMS_DIR").unwrap_or_else(|_| "./params".to_string()))
        .join(format!("kzg_bn254_{k}.srs"))
}

/// What it means that making the aggregated proof panicked — the SDK checks
/// every proof it makes against the key, and a failed check is
/// `assertion failed: accept` — and what is done about it. The inner snark
/// is not the cause: the aggregation circuit only carries its accumulator,
/// and a wrong one is caught by the verifier's pairing, on chain. The
/// ceremony `srs` may be — its loader checks its head, not its points.
///
/// - A key read from the pk cache is damaged in place, or the ceremony is: the
///   slot is set aside, so that the next run generates the key.
/// - A key this run generated means a damaged ceremony, or a keygen and prover
///   that disagree — a bug or a hardware fault. It is published already, so its
///   slot is set aside too: the next preflight would otherwise take it for a
///   good one.
fn self_check_failed(
    pk_cache_dir: Option<&Path>,
    slot: Option<&str>,
    source: KeySource,
    srs: &Path,
    panic: &str,
) -> anyhow::Error {
    let failed = format!(
        "making the aggregated proof failed ({panic}); the SDK checks every proof it makes \
         against the key it was made with"
    );
    let (why, meta) = match source {
        KeySource::Loaded {
            meta,
        } => (
            format!(
                "The key was read from the pk cache: it is damaged in place — the right length, \
                 wrong bytes — or the points of the ceremony file {} are.",
                srs.display()
            ),
            meta,
        ),
        KeySource::Generated {
            meta,
        } => (
            format!(
                "This run generated the key itself: the ceremony file {} is damaged past the head \
                 its loader checks — then every key made with it fails the same way, and it has \
                 to be downloaded again — or keygen and prover disagree, a bug or a hardware \
                 fault.",
                srs.display()
            ),
            meta,
        ),
    };
    let (Some(dir), Some(stem)) = (pk_cache_dir, slot) else {
        return anyhow::anyhow!("{failed}. {why}");
    };
    let pk = dir.join(format!("{stem}.pk"));
    let then = if crate::aggregator_cache::set_aside_slot(dir, stem, meta) {
        format!(
            "Its slot is set aside as {stem}.meta.json.damaged: the next run that needs it \
             removes both files and generates the key again; removing {} and that file now frees \
             the key's space at once.",
            pk.display()
        )
    } else {
        format!(
            "Its slot could not be set aside: remove {} and {} to have both generated again.",
            pk.display(),
            dir.join(format!("{stem}.meta.json")).display()
        )
    };
    anyhow::anyhow!("{failed}. {why} {then}")
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
        slot: pk_cache_slot,
        source,
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
    // A file of its own, not stderr: the callers forward stderr into their
    // logs, and this dump would bury everything else there.
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

    // The SDK verifies the proof it makes against the key and panics when it
    // does not verify. A key from the pk cache that is wrong in place — the
    // right length, bytes that still read — gets this far: said, and its
    // slot set aside, rather than left as a bare assertion.
    let evm_proof = proof_or(
        || gen_evm_proof_shplonk(&params_outer, &pk, prover_circuit, instances.clone()),
        |panic| {
            self_check_failed(
                pk_cache_dir,
                pk_cache_slot.as_deref(),
                source,
                &srs_path(config.k_outer),
                panic,
            )
        },
    )?;

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
        pk_cache_slot,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_while_proving_is_an_error_with_its_message() {
        let err = proof_or(
            || -> Vec<u8> { panic!("assertion failed: accept") },
            |panic| anyhow::anyhow!("explained: {panic}"),
        )
        .expect_err("the panic is caught");
        assert_eq!(err.to_string(), "explained: assertion failed: accept");
        assert_eq!(proof_or(|| 7, |_| unreachable!()).unwrap(), 7);
    }

    /// A pk cache under the temp dir with slot `S`, and its meta's identity.
    fn slot(tag: &str) -> (PathBuf, Option<(u64, u64)>) {
        use std::os::unix::fs::MetadataExt as _;
        let dir = std::env::temp_dir().join(format!("self_check_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("S.pk"), b"key").unwrap();
        std::fs::write(dir.join("S.meta.json"), b"{}").unwrap();
        let md = std::fs::metadata(dir.join("S.meta.json")).unwrap();
        (dir, Some((md.dev(), md.ino())))
    }

    #[test]
    fn a_failed_self_check_sets_the_slot_aside_and_names_the_ceremony() {
        let srs = Path::new("/params/kzg_bn254_21.srs");
        for (source, why) in [
            (
                KeySource::Loaded {
                    meta: None,
                },
                "read from the pk cache",
            ),
            (
                KeySource::Generated {
                    meta: None,
                },
                "This run generated the key itself",
            ),
        ] {
            let (dir, read) = slot("aside");
            let source = match source {
                KeySource::Loaded {
                    ..
                } => KeySource::Loaded {
                    meta: read,
                },
                KeySource::Generated {
                    ..
                } => KeySource::Generated {
                    meta: read,
                },
            };
            let msg = self_check_failed(
                Some(&dir),
                Some("S"),
                source,
                srs,
                "assertion failed: accept",
            )
            .to_string();
            assert!(
                msg.contains("assertion failed: accept")
                    && msg.contains(why)
                    && msg.contains("/params/kzg_bn254_21.srs")
                    && msg.contains("set aside as S.meta.json.damaged"),
                "{msg}"
            );
            assert!(!dir.join("S.meta.json").exists(), "{why}: set aside");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn a_slot_that_cannot_be_set_aside_is_named_for_removal() {
        // Its meta is not the one the run read: someone else's by now.
        let (dir, _) = slot("not_aside");
        let msg = self_check_failed(
            Some(&dir),
            Some("S"),
            KeySource::Loaded {
                meta: Some((0, 0)),
            },
            Path::new("/p/kzg_bn254_21.srs"),
            "x",
        )
        .to_string();
        assert!(msg.contains("could not be set aside: remove"), "{msg}");
        assert!(dir.join("S.meta.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_panic_with_a_formatted_message_keeps_it() {
        let err = proof_or(
            || -> u8 { panic!("{} {}", "formatted", 7) },
            |panic| anyhow::anyhow!("{panic}"),
        )
        .expect_err("caught");
        assert_eq!(err.to_string(), "formatted 7");
    }
}
