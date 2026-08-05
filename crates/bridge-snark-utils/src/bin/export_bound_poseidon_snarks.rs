//! Re-prove the bound block scenario with Poseidon transcript and emit
//! snark-verifier [`Snark`] files for the R15 aggregator pipeline.

use std::path::PathBuf;

use anyhow::Context;
use bridge_prover_lib::{
    keys::KeyManager,
    layer_prover::generate_layer_proof_with_input_and_transcript as generate_layer_hashes_proof_with_transcript,
    prover::{
        generate_fallback_proof_with_transcript, generate_primary_proof_with_transcript, ProofOutput,
    },
    transcript::TranscriptKind,
    verifier::{verify_fallback_proof_with_transcript, verify_layer_proof_with_transcript},
    Fr,
};
use bridge_snark_utils::{
    compose_layer_hashes_input, halo2_snark::export_poseidon_snark_with_srs_k,
    load_bound_witness_cache, proof_export::save_instances_binary,
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
    /// SRS degree override for the snark-verifier compile step. `None` = use
    /// `config.k`. Required for `layer_hashes`, whose VK was keygen'd at K=20
    /// even though `config.k = 17` (see
    /// `LayerHashesKeyManager::KEYGEN_SRS_K`).
    srs_k_override: Option<u32>,
}

const CIRCUITS: &[CircuitExport<'static>] = &[
    CircuitExport {
        name: "primary",
        vk_key: "primary_vk.bin",
        config_key: "primary_config_params.json",
        srs_k_override: None,
    },
    CircuitExport {
        name: "fallback",
        vk_key: "fallback_vk.bin",
        config_key: "fallback_config_params.json",
        srs_k_override: None,
    },
    CircuitExport {
        name: "layer_hashes",
        vk_key: "layer_hashes_vk.bin",
        config_key: "layer_hashes_config_params.json",
        srs_k_override: Some(20),
    },
];

/// Circuit 1A/1B public-instance vector `[block_id, bk_set_poseidon,
/// block_seq_no, last_seen]`, reconstructed from the prover-lib
/// [`ProofOutput`] (which exposes the raw Fr fields rather than a packed
/// vector).
fn attestation_instances(p: &ProofOutput) -> [Fr; 4] {
    [
        p.block_id_fr,
        p.bk_set_commitment_fr,
        Fr::from(p.block_seq_no as u64),
        Fr::from(p.last_seen_block_seqno as u64),
    ]
}

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

    // One monolithic KeyManager drives all three circuits (per-circuit
    // sub-managers own the right K / SRS each).
    let mut km = KeyManager::new(&params_dir);

    km.ensure_primary_keys(&bound.bk_set)?;
    km.load_primary_pk()?;
    let primary = generate_primary_proof_with_transcript(
        &km,
        &bound.attestation_primary_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    km.unload_primary_pk();
    let primary_proof_path = snark_dir.join("primary.proof.bin");
    std::fs::write(&primary_proof_path, &primary.proof_bytes)?;

    let fallback_bytes = bound
        .attestation_fallback_bytes
        .as_ref()
        .context("bound data missing fallback attestation")?;
    km.ensure_fallback_keys(&bound.bk_set)?;
    km.load_fallback_pk()?;
    let fallback = generate_fallback_proof_with_transcript(
        &km,
        &bound.attestation_primary_bytes,
        fallback_bytes,
        &bound.bk_set,
        bound.last_seen_block_seqno,
        TranscriptKind::Poseidon,
    )?;
    let fallback_proof_path = snark_dir.join("fallback.proof.bin");
    std::fs::write(&fallback_proof_path, &fallback.proof_bytes)?;
    {
        let ok = verify_fallback_proof_with_transcript(
            &km,
            &fallback.proof_bytes,
            &attestation_instances(&fallback),
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
    km.unload_fallback_pk();

    km.ensure_layer_keys()?;
    km.load_layer_pk()?;
    {
        // Diagnostic: prove + verify the SAME bound layer witness under Blake2b.
        // If this PASSES while Poseidon FAILS → transcript-specific bug.
        // If this also FAILS → the bound witness/keygen is the problem (not the
        // transcript and not the snark-verifier aggregator).
        use bridge_prover_lib::layer_prover::generate_layer_proof_with_input;
        use bridge_prover_lib::verifier::verify_layer_proof;
        let blake = generate_layer_proof_with_input(&km, compose_layer_hashes_input(&bound))?;
        let ok_blake = verify_layer_proof(&km, &blake.proof_bytes, &blake.instances);
        println!(
            "DIAG layer_hashes (Blake2b round-trip, fresh keys + bound witness): {}",
            if ok_blake { "PASS" } else { "FAIL" }
        );
    }
    let layer = generate_layer_hashes_proof_with_transcript(
        &km,
        compose_layer_hashes_input(&bound),
        TranscriptKind::Poseidon,
    )?;
    let layer_proof_path = snark_dir.join("layer_hashes.proof.bin");
    std::fs::write(&layer_proof_path, &layer.proof_bytes)?;
    {
        let ok = verify_layer_proof_with_transcript(
            &km,
            &layer.proof_bytes,
            &layer.instances,
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
    km.unload_layer_pk();

    let outputs: [(&str, &PathBuf, Vec<_>); 3] = [
        ("primary", &primary_proof_path, attestation_instances(&primary).to_vec()),
        ("fallback", &fallback_proof_path, attestation_instances(&fallback).to_vec()),
        ("layer_hashes", &layer_proof_path, layer.instances.to_vec()),
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
        export_poseidon_snark_with_srs_k(
            &params_dir.join(spec.vk_key),
            &params_dir.join(spec.config_key),
            spec.srs_k_override,
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
