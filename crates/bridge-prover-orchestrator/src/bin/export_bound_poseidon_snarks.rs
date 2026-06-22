//! Re-prove the bound block scenario with Poseidon transcript and emit
//! snark-verifier [`Snark`] files for the R15 aggregator pipeline.

use std::path::PathBuf;

use anyhow::Context;
use bridge_prover_lib::keys::KeyManager as PrimaryKeyManager;
use bridge_prover_orchestrator::{
    compose_layer_hashes_input,
    generate_fallback_proof_with_transcript, generate_layer_hashes_proof_with_transcript,
    generate_primary_proof_with_transcript,
    halo2_snark::export_poseidon_snark,
    halo2_tvm_bundle::TranscriptKind,
    layer_hashes_keys::{LayerHashesKeyManager, LayerHashesReferenceWitness},
    load_bound_witness_cache, proof_export::save_instances_binary,
    FallbackKeyManager,
};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "export-bound-poseidon-snarks",
    about = "Poseidon re-prove of bound 1A/1B/2 + Snark export for aggregator"
)]
struct Args {
    #[arg(long, default_value = "../../params")]
    params_dir: String,
    #[arg(long, default_value = "../../proofs/bound")]
    bound_dir: String,
    #[arg(long, default_value = "../../proofs/bound/poseidon-snark")]
    snark_dir: String,
}

struct CircuitExport<'a> {
    name: &'a str,
    vk_key: &'a str,
    config_key: &'a str,
}

const CIRCUITS: &[CircuitExport<'static>] = &[
    CircuitExport {
        name: "primary",
        vk_key: "primary_vk.bin",
        config_key: "primary_config_params.json",
    },
    CircuitExport {
        name: "fallback",
        vk_key: "fallback_vk.bin",
        config_key: "fallback_config_params.json",
    },
    CircuitExport {
        name: "layer_hashes",
        vk_key: "layer_hashes_vk.bin",
        config_key: "layer_hashes_config_params.json",
    },
];

fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let args = Args::parse();
    let params_dir = PathBuf::from(&args.params_dir);
    let bound_dir = PathBuf::from(&args.bound_dir);
    let snark_dir = PathBuf::from(&args.snark_dir);
    std::fs::create_dir_all(&snark_dir)?;

    let witness_path = bound_dir.join("bound_witness.bin");
    let bound = load_bound_witness_cache(&witness_path).with_context(|| {
        format!(
            "load {} (run export-bound-block-proofs first)",
            witness_path.display()
        )
    })?;

    let mut primary_km = PrimaryKeyManager::new(&params_dir);
    primary_km.ensure_primary_keys(&bound.bk_set)?;
    primary_km.load_primary_pk()?;
    let primary = generate_primary_proof_with_transcript(
        &primary_km,
        &bound.attestation_primary_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    primary_km.unload_primary_pk();
    let primary_proof_path = snark_dir.join("primary.proof.bin");
    std::fs::write(&primary_proof_path, &primary.proof_bytes)?;

    let fallback_bytes = bound
        .attestation_fallback_bytes
        .as_ref()
        .context("bound data missing fallback attestation")?;
    let mut fallback_km = FallbackKeyManager::new(&params_dir);
    fallback_km.ensure_keys(&bound.bk_set)?;
    let fallback = generate_fallback_proof_with_transcript(
        &fallback_km,
        &bound.attestation_primary_bytes,
        fallback_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    let fallback_proof_path = snark_dir.join("fallback.proof.bin");
    std::fs::write(&fallback_proof_path, &fallback.proof_bytes)?;
    {
        use bridge_prover_orchestrator::verify_fallback_proof_with_transcript;
        let ok = verify_fallback_proof_with_transcript(
            &fallback_km,
            &fallback.proof_bytes,
            &fallback.instances(),
            TranscriptKind::Poseidon,
        );
        println!("SELF_VERIFY fallback (Poseidon native): {}", if ok { "PASS" } else { "FAIL" });
        anyhow::ensure!(
            ok,
            "fallback Poseidon inner snark failed native verification — refusing to export an \
             invalid snark (stale/ mismatched keys?). Regenerate fallback keys against the \
             current bound witness."
        );
    }

    let mut layer_km = LayerHashesKeyManager::new(&params_dir);
    layer_km.ensure_keys(&LayerHashesReferenceWitness {
        layer_hashes_preimage: bound.layer_hashes_preimage,
        merkle_siblings: bound.merkle_siblings,
        prev_max_level_layer_hash: bound.prev_max_level_layer_hash,
        num_prev_chain_steps: bound.num_prev_chain_steps,
        prev_chain_proofs: bound.prev_chain_proofs.clone(),
        bk_set_poseidon_hash: bound.bk_set_poseidon_fr,
    })?;
    {
        // Diagnostic: prove + verify the SAME bound layer witness under Blake2b.
        // If this PASSES while Poseidon FAILS → transcript-specific bug.
        // If this also FAILS → the bound witness/keygen is the problem (not the
        // transcript and not the snark-verifier aggregator).
        use bridge_prover_orchestrator::{
            generate_layer_hashes_proof, verify_layer_hashes_proof,
        };
        let blake = generate_layer_hashes_proof(&layer_km, compose_layer_hashes_input(&bound))?;
        let ok_blake = verify_layer_hashes_proof(&layer_km, &blake.proof_bytes, &blake.instances());
        println!(
            "DIAG layer_hashes (Blake2b round-trip, fresh keys + bound witness): {}",
            if ok_blake { "PASS" } else { "FAIL" }
        );
    }
    let layer = generate_layer_hashes_proof_with_transcript(
        &layer_km,
        compose_layer_hashes_input(&bound),
        TranscriptKind::Poseidon,
    )?;
    let layer_proof_path = snark_dir.join("layer_hashes.proof.bin");
    std::fs::write(&layer_proof_path, &layer.proof_bytes)?;
    {
        use bridge_prover_orchestrator::verify_layer_hashes_proof_with_transcript;
        let ok = verify_layer_hashes_proof_with_transcript(
            &layer_km,
            &layer.proof_bytes,
            &layer.instances(),
            TranscriptKind::Poseidon,
        );
        println!("SELF_VERIFY layer_hashes (Poseidon native): {}", if ok { "PASS" } else { "FAIL" });
        anyhow::ensure!(
            ok,
            "layer-hashes Poseidon inner snark failed native verification — refusing to export an \
             invalid snark. This is almost always stale/mismatched layer keys: \
             `ensure_keys` short-circuits on cached vk/pk, so a bound witness regenerated after \
             the keys is proved against the wrong VK. Delete params/layer_hashes_{{vk,pk}}.bin + \
             layer_hashes_config_params.json and re-run to keygen against the current witness."
        );
    }

    let outputs: [(&str, &PathBuf, Vec<_>); 3] = [
        ("primary", &primary_proof_path, primary.instances().to_vec()),
        ("fallback", &fallback_proof_path, fallback.instances().to_vec()),
        ("layer_hashes", &layer_proof_path, layer.instances().to_vec()),
    ];

    for spec in CIRCUITS {
        let (_, proof_path, instances) = outputs
            .iter()
            .find(|(n, _, _)| *n == spec.name)
            .context("internal circuit list mismatch")?;

        let instances_path = snark_dir.join(format!("{}.instances.bin", spec.name));
        save_instances_binary(instances, &instances_path)?;
        let out_snark = snark_dir.join(format!("{}.snark", spec.name));

        info!(circuit = spec.name, "exporting Poseidon Snark");
        export_poseidon_snark(
            &params_dir.join(spec.vk_key),
            &params_dir.join(spec.config_key),
            proof_path,
            instances,
            &out_snark,
        )?;
        println!(
            "OK: {} -> {} ({} B)",
            spec.name,
            out_snark.display(),
            std::fs::metadata(&out_snark)?.len()
        );
    }

    println!("OK: Poseidon snarks in {}", snark_dir.display());
    Ok(())
}
