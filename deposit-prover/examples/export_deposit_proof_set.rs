//! Export a **set of opcode-consumable deposit proofs that all verify against a
//! single, universal VkBlob** — the artefact set the AN `ZKHALO2VERIFYWITHVK`
//! opcode test consumes.
//!
//! ## Universal VK (one deployed VK verifies every deposit proof)
//!
//! The deposit circuit is fixed-size: every variable-length input is padded to a
//! constant width in-circuit (the receipt + MPT proof via axiom-eth's
//! `value_max_byte_len` / `max_depth`; the block header via
//! `circuit_v2::MAX_BLOCK_HEADER_BYTES`). So the constraint system — hence the
//! verifying key — is identical for every block. We therefore run keygen **once**
//! (from the first input), pinning `EthCircuitParams` + `RlcThreadBreakPoints` +
//! the proving key, then build every prover circuit with those same pinned params
//! + break points. Each proof is self-verified against the one shared VK
//! (`pk.get_vk()`), which is exactly the VkBlob written to disk.
//!
//! Layout produced under `--set-dir`:
//!   <set-dir>/deposit_vk_blob.bin             (shared, v2 RLC VkBlob)
//!   <set-dir>/deposit_eth_circuit_params.json (shared EthCircuitParams)
//!   <set-dir>/proof_NN/proof.bin              (Blake2b SHPLONK proof)
//!   <set-dir>/proof_NN/public_inputs.bin      (11 × 32-byte LE Fr)
//!
//! Each `proof_NN/input.json` must already exist (written by
//! `fetch_deposit_data`). Run with:
//!   cargo run --release --example export_deposit_proof_set -- \
//!     --set-dir fixtures/deposit_10proofs --count 10 \
//!     --degree 18 --max-data-byte-len 256 --max-log-num 20

#[path = "../../crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs"]
mod halo2_tvm_bundle;

use std::{fs, path::Path};

use axiom_eth::utils::eth_circuit::{create_circuit, EthCircuitParams};
use clap::Parser;
use deposit_prover::{
    circuit_v2::DepositEventCircuitV2,
    prover::{get_default_params, load_kzg_params_from_trusted_setup, CircuitConfig},
    types::DepositProofInput,
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{create_proof, keygen_pk, keygen_vk, verify_proof},
        poly::{
            commitment::{Params, ParamsProver},
            kzg::{
                commitment::{KZGCommitmentScheme, ParamsKZG},
                multiopen::{ProverSHPLONK, VerifierSHPLONK},
                strategy::SingleStrategy,
            },
        },
        transcript::{
            Blake2bRead, Blake2bWrite, Challenge255, TranscriptReadBuffer, TranscriptWriterBuffer,
        },
    },
};
use halo2_tvm_bundle::{CircuitShape, VkBlob};
use rand::rngs::OsRng;
use snark_verifier_sdk::CircuitExt;

#[derive(Parser, Debug)]
#[command(name = "export-deposit-proof-set")]
struct Args {
    /// Directory containing `proof_NN/input.json`; shared VkBlob + per-proof
    /// outputs are written back into it.
    #[arg(long)]
    set_dir: String,
    /// Number of proofs (`proof_00`..`proof_{count-1}`).
    #[arg(long, default_value = "10")]
    count: usize,
    #[arg(long, default_value = "18")]
    degree: u32,
    #[arg(long, default_value = "256")]
    max_data_byte_len: usize,
    #[arg(long, default_value = "20")]
    max_log_num: usize,
}

fn load_input(set_dir: &str, i: usize) -> anyhow::Result<DepositProofInput> {
    let path = format!("{}/proof_{:02}/input.json", set_dir, i);
    let json = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {path}: {e} (run fetch_deposit_data first)"))?;
    Ok(serde_json::from_str(&json)?)
}

fn main() -> anyhow::Result<()> {
    println!("=== Export deposit proof set (single shared VkBlob) ===\n");
    let args = Args::parse();
    let config = CircuitConfig {
        degree: args.degree,
        max_data_byte_len: args.max_data_byte_len,
        max_log_num: args.max_log_num,
        topic_num_bounds: (0, 4),
    };

    let srs = load_kzg_params_from_trusted_setup(args.degree).map_err(|e| anyhow::anyhow!("{e}"))?;

    // ---- Keygen ONCE from proof_00, pinning params + break points + pk ----
    println!("Keygen (once) from proof_00/input.json ...");
    let ref_input = load_input(&args.set_dir, 0)?;
    let mut kcircuit = create_circuit(
        CircuitBuilderStage::Keygen,
        get_default_params(),
        DepositEventCircuitV2::new(ref_input, &config),
    );
    kcircuit.mock_fulfill_keccak_promises(None);
    let eth_params: EthCircuitParams = kcircuit.calculate_params();
    println!(
        "  pinned params: k={} num_rlc_columns={}",
        eth_params.rlc.base.k, eth_params.rlc.num_rlc_columns
    );
    let vk = keygen_vk(&srs, &kcircuit).map_err(|e| anyhow::anyhow!("keygen_vk: {e:?}"))?;

    // Shared VkBlob (v2 RLC) — written before pk consumes `vk`.
    let eth_config_json = serde_json::to_vec(&eth_params)?;
    let blob = VkBlob::from_native_rlc(eth_config_json.clone(), &vk)?;
    assert_eq!(blob.shape(), CircuitShape::Rlc, "must be RLC shape");
    let blob_bytes = blob.to_bytes()?;
    fs::write(format!("{}/deposit_vk_blob.bin", args.set_dir), &blob_bytes)?;
    fs::write(
        format!("{}/deposit_eth_circuit_params.json", args.set_dir),
        &eth_config_json,
    )?;
    println!("  wrote shared VkBlob ({} bytes)\n", blob_bytes.len());

    let pk = keygen_pk(&srs, vk, &kcircuit).map_err(|e| anyhow::anyhow!("keygen_pk: {e:?}"))?;
    let break_points = kcircuit.break_points();
    drop(kcircuit);

    // ---- Prove every input with the SAME pinned params + break points ----
    let mut ok = 0usize;
    for i in 0..args.count {
        let dir = format!("{}/proof_{:02}", args.set_dir, i);
        if !Path::new(&format!("{dir}/input.json")).exists() {
            println!("proof_{i:02}: SKIP (no input.json)");
            continue;
        }
        let input = load_input(&args.set_dir, i)?;
        let circuit = create_circuit(
            CircuitBuilderStage::Prover,
            eth_params.rlc.clone(),
            DepositEventCircuitV2::new(input, &config),
        )
        .use_break_points(break_points.clone());
        circuit.mock_fulfill_keccak_promises(None);

        let instances = circuit.instances();
        anyhow::ensure!(
            instances.len() == 1 && instances[0].len() == 11,
            "proof_{i:02}: expected 11 public inputs, got {:?}",
            instances.iter().map(|c| c.len()).collect::<Vec<_>>()
        );
        let inst0 = instances[0].clone();

        let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(Vec::new());
        create_proof::<
            KZGCommitmentScheme<Bn256>,
            ProverSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            _,
            Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
            _,
        >(&srs, &pk, &[circuit], &[&[inst0.as_slice()]], OsRng, &mut transcript)
        .map_err(|e| anyhow::anyhow!("proof_{i:02}: create_proof: {e:?}"))?;
        let proof = transcript.finalize();

        let mut pubin = Vec::with_capacity(inst0.len() * 32);
        for fr in &inst0 {
            pubin.extend_from_slice(fr.to_bytes().as_ref());
        }
        fs::write(format!("{dir}/proof.bin"), &proof)?;
        fs::write(format!("{dir}/public_inputs.bin"), &pubin)?;

        // Self-verify against the SHARED vk (pk.get_vk()) — the opcode's check.
        let strategy = SingleStrategy::new(&srs);
        let mut vt = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(proof.as_slice());
        verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
            SingleStrategy<'_, Bn256>,
        >(
            srs.verifier_params(),
            pk.get_vk(),
            strategy,
            &[&[inst0.as_slice()]],
            &mut vt,
        )
        .map_err(|e| anyhow::anyhow!("proof_{i:02}: verify against shared VK FAILED: {e:?}"))?;
        println!(
            "proof_{i:02}: proof={}B pubin={}B -> VERIFIED against shared VkBlob",
            proof.len(),
            pubin.len()
        );
        ok += 1;
    }

    println!("\nRESULT: {ok}/{} proofs verify against the single shared VkBlob.", args.count);
    Ok(())
}
