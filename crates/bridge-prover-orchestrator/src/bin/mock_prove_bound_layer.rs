//! Diagnostic: run `MockProver` on Circuit 2 with a freshly-built *bound* layer
//! witness to confirm whether it satisfies the circuit constraints. Fast
//! (K=17, no SRS proving). Pinpoints any violated gate.
//!
//! This builds the bound scenario in-process via `build_bound_test_data` so it
//! exercises the current `promote_bridge_test_data` mapping (incl. the
//! `num_prev_chain_steps` → total-active-steps conversion), independent of any
//! cached `bound_witness.bin`.

use std::path::PathBuf;

use bridge_prover_orchestrator::{
    build_bound_test_data, compose_layer_hashes_input,
    generate_layer_hashes_proof,
    layer_hashes_keys::{LayerHashesKeyManager, LayerHashesReferenceWitness},
    mock_prove_layer_hashes, verify_layer_hashes_proof,
};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mock-prove-bound-layer")]
struct Args {
    #[arg(long, default_value = "params")]
    params_dir: String,
    #[arg(long, default_value_t = 10)]
    signers: usize,
    #[arg(long, default_value_t = 5)]
    num_layers: usize,
    #[arg(long, default_value_t = 3)]
    num_chain_steps: usize,
    /// Also run a full native SHPLONK prove+verify (Blake2b) after MockProver.
    #[arg(long, default_value_t = false)]
    native: bool,
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

    println!(
        "=== Build bound scenario: signers={} num_layers={} num_chain_steps={} ===",
        args.signers, args.num_layers, args.num_chain_steps
    );
    let bound = build_bound_test_data(args.signers, args.num_layers, args.num_chain_steps, true)?;
    println!(
        "bound.num_prev_chain_steps (circuit total-active) = {}",
        bound.num_prev_chain_steps
    );
    println!("prev_chain_proofs len = {}", bound.prev_chain_proofs.len());

    let mut layer_km = LayerHashesKeyManager::new(&params_dir);
    layer_km.ensure_keys(&LayerHashesReferenceWitness {
        layer_hashes_preimage: bound.layer_hashes_preimage,
        merkle_siblings: bound.merkle_siblings,
        prev_max_level_layer_hash: bound.prev_max_level_layer_hash,
        num_prev_chain_steps: bound.num_prev_chain_steps,
        prev_chain_proofs: bound.prev_chain_proofs.clone(),
        bk_set_poseidon_hash: bound.bk_set_poseidon_fr,
    })?;

    match mock_prove_layer_hashes(&layer_km, compose_layer_hashes_input(&bound)) {
        Ok(()) => {
            println!("MOCKPROVER: PASS — bound witness satisfies all Circuit 2 constraints");
        }
        Err(e) => {
            println!("MOCKPROVER: FAIL — bound witness violates constraints:");
            println!("{e}");
            std::process::exit(1);
        }
    }

    if args.native {
        println!("=== Native SHPLONK prove + verify (Blake2b) ===");
        let proof = generate_layer_hashes_proof(&layer_km, compose_layer_hashes_input(&bound))?;
        let ok = verify_layer_hashes_proof(&layer_km, &proof.proof_bytes, &proof.instances());
        if ok {
            println!(
                "NATIVE_VERIFY: PASS ({} bytes, {} PI)",
                proof.proof_bytes.len(),
                proof.instances().len()
            );
        } else {
            println!("NATIVE_VERIFY: FAIL");
            std::process::exit(1);
        }
    }
    Ok(())
}
