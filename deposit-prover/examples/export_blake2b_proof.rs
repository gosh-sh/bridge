//! Export a **Blake2b-transcript** SHPLONK proof for the deposit circuit —
//! the proof flavour the AN `ZKHALO2VERIFYWITHVK` opcode consumes.
//!
//! `deposit_prover::prover::generate_proof` produces a `snark-verifier-sdk`
//! `Snark` whose SHPLONK proof uses a **Poseidon** transcript (for the EVM
//! aggregation path). The opcode commits to **Blake2b** (Variant A, frozen
//! 2026-05-22), so that proof is not opcode-consumable. This tool runs the raw
//! halo2 `create_proof::<_, ProverSHPLONK, _, Blake2bWrite, _>` over the SAME
//! prover circuit + PK, emitting:
//!   * `--proof-out`    : raw SHPLONK proof bytes (Blake2b, no header) — the
//!     `proof_cell` operand.
//!   * `--pubin-out`    : the 11 public inputs as `N × 32` LE `Fr` — the
//!     `public_inputs_cell` operand.
//!
//! It then verifies the (vk, instances, proof) triple in-process with
//! `verify_proof::<KZG, VerifierSHPLONK, _, Blake2bRead, SingleStrategy>`
//! against `srs.verifier_params()` — byte-for-byte the check the opcode handler
//! runs on the AN node.
//!
//! Usage:
//!   cargo run --release --example export_blake2b_proof -- \
//!     --input /tmp/deposit_e2e/deposit_proof_input.json \
//!     --proof-out /tmp/deposit_e2e/deposit_proof_blake2b.bin \
//!     --pubin-out /tmp/deposit_e2e/deposit_public_inputs.bin \
//!     --degree 18 --max-data-byte-len 256 --max-log-num 20

use std::{fs, path::Path};

use axiom_eth::utils::eth_circuit::create_circuit;
use clap::Parser;
use deposit_prover::{
    circuit_v2::DepositEventCircuitV2,
    prover::{
        get_default_params, get_or_create_proving_key, load_kzg_params_from_trusted_setup,
        CircuitConfig,
    },
    types::DepositProofInput,
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{create_proof, verify_proof},
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
use rand::rngs::OsRng;
use snark_verifier_sdk::CircuitExt;

#[derive(Parser, Debug)]
#[command(name = "export-blake2b-proof")]
#[command(about = "Export a Blake2b-transcript SHPLONK proof for ZKHALO2VERIFYWITHVK", long_about = None)]
struct Args {
    #[arg(long)]
    input: String,
    #[arg(long, default_value = "deposit_proof_blake2b.bin")]
    proof_out: String,
    #[arg(long, default_value = "deposit_public_inputs.bin")]
    pubin_out: String,
    #[arg(long, default_value = "18")]
    degree: u32,
    #[arg(long, default_value = "256")]
    max_data_byte_len: usize,
    #[arg(long, default_value = "20")]
    max_log_num: usize,
}

fn main() -> anyhow::Result<()> {
    println!("=== Export Blake2b SHPLONK proof (opcode-consumable) ===\n");
    let args = Args::parse();

    let json = fs::read_to_string(&args.input)?;
    let input: DepositProofInput = serde_json::from_str(&json)?;
    let config = CircuitConfig {
        degree: args.degree,
        max_data_byte_len: args.max_data_byte_len,
        max_log_num: args.max_log_num,
        topic_num_bounds: (0, 4),
    };

    // SRS (`data/kzg_params_{k}.srs` from `download_trusted_setup.sh`).
    let k = config.degree;
    let params = load_kzg_params_from_trusted_setup(k).map_err(|e| anyhow::anyhow!("{e}"))?;

    // PK + calculated params + break points — identical machinery to
    // generate_proof.
    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    fs::create_dir_all("data")?;
    println!("Building/loading proving key...");
    let (pk, circuit_params, break_points) =
        get_or_create_proving_key(&params, &input, &config, Path::new(&pk_path_str))
            .map_err(|e| anyhow::anyhow!("get_or_create_proving_key: {e}"))?;

    // Prover-stage circuit with the keygen break points (mirrors generate_proof).
    let circuit_input = DepositEventCircuitV2::new(input, &config);
    let circuit = create_circuit(CircuitBuilderStage::Prover, circuit_params, circuit_input)
        .use_break_points(break_points);
    circuit.mock_fulfill_keccak_promises(None);

    let instances: Vec<Vec<Fr>> = circuit.instances();
    anyhow::ensure!(
        instances.len() == 1 && instances[0].len() == 11,
        "expected 1 instance column of 11 inputs, got {:?}",
        instances.iter().map(|c| c.len()).collect::<Vec<_>>()
    );
    let inst0 = instances[0].clone();

    // Raw Blake2b create_proof.
    println!("Generating Blake2b SHPLONK proof...");
    let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(Vec::new());
    create_proof::<
        KZGCommitmentScheme<Bn256>,
        ProverSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        _,
        Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
        _,
    >(
        &params,
        &pk,
        &[circuit],
        &[&[inst0.as_slice()]],
        OsRng,
        &mut transcript,
    )
    .map_err(|e| anyhow::anyhow!("create_proof failed: {e:?}"))?;
    let proof: Vec<u8> = transcript.finalize();
    println!("Proof: {} bytes (Blake2b transcript)", proof.len());

    // Public inputs payload: strict 32-byte LE Fr, no header.
    let mut pubin = Vec::with_capacity(inst0.len() * 32);
    for fr in &inst0 {
        pubin.extend_from_slice(fr.to_bytes().as_ref());
    }

    fs::write(&args.proof_out, &proof)?;
    fs::write(&args.pubin_out, &pubin)?;
    println!(
        "Wrote proof -> {}\nWrote public inputs ({} bytes = {} Fr) -> {}\n",
        args.proof_out,
        pubin.len(),
        pubin.len() / 32,
        args.pubin_out
    );

    // In-process verification = the opcode handler's exact check.
    println!("Verifying (opcode handler path: Blake2bRead + VerifierSHPLONK)...");
    let vk = pk.get_vk();
    let strategy = SingleStrategy::new(&params);
    let mut vtranscript = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(proof.as_slice());
    verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
        SingleStrategy<'_, Bn256>,
    >(
        params.verifier_params(),
        vk,
        strategy,
        &[&[inst0.as_slice()]],
        &mut vtranscript,
    )
    .map_err(|e| anyhow::anyhow!("verify_proof REJECTED the Blake2b proof: {e:?}"))?;

    println!("  OK: Blake2b SHPLONK proof VERIFIED against the deposit VK.");
    println!("\nRESULT: PASS — opcode-consumable (vk_blob, public_inputs, proof) triple is ready.");
    Ok(())
}
