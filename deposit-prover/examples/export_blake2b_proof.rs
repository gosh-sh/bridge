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
//!   * `--pubin-out`    : the 12 public inputs as `N × 32` LE `Fr` — the
//!     `public_inputs_cell` operand.
//!   * `--keccak-proof-out` : the keccak shard proof (Blake2b, no header) for
//!     the keccak table the deposit proof loads; its only public input is
//!     public input 11 of the deposit proof. `finalizeDeposit` verifies both.
//!   * `--keccak-vk-out` (optional): the keccak shard `VkBlob` that proof
//!     verified under — compare it with the `KECCAK_VK_BLOB` the bridge embeds.
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
//!     --keccak-proof-out /tmp/deposit_e2e/keccak_proof_blake2b.bin \
//!     --degree 18 --max-data-byte-len 2048 --max-log-num 20

use std::{fs, path::Path};

use axiom_eth::utils::{
    component::promise_loader::single::PromiseLoaderParams, eth_circuit::EthCircuitImpl,
};
use clap::Parser;
use deposit_prover::{
    circuit_v2::{DepositEventCircuitV2, PRODUCTION_MAX_DATA_BYTE_LEN, PRODUCTION_MAX_LOG_NUM},
    keccak_shard::{self, PROMISE_COMMIT_INDEX},
    provable::exit_if_unprovable,
    prover::{
        get_or_create_proving_key, load_kzg_params_from_trusted_setup, CircuitConfig,
        FIXED_KECCAK_CAPACITY, PRODUCTION_DEGREE,
    },
    types::DepositProofInput,
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{create_proof, verify_proof},
        poly::{
            commitment::ParamsProver,
            kzg::{
                commitment::KZGCommitmentScheme,
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
    #[arg(long, default_value = "keccak_proof_blake2b.bin")]
    keccak_proof_out: String,
    #[arg(long)]
    keccak_vk_out: Option<String>,
    #[arg(long, default_value_t = PRODUCTION_DEGREE)]
    degree: u32,
    #[arg(long, default_value_t = PRODUCTION_MAX_DATA_BYTE_LEN)]
    max_data_byte_len: usize,
    #[arg(long, default_value_t = PRODUCTION_MAX_LOG_NUM)]
    max_log_num: usize,

    /// Source network (not baked into VK; proven chainId is a PI). Must be in
    /// `SUPPORTED_DEPOSIT_CHAIN_IDS` (e.g. Sepolia=11155111) and must match the
    /// chain the loaded witness actually proves.
    #[arg(long)]
    chain_id: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    println!("=== Export Blake2b SHPLONK proof (opcode-consumable) ===\n");
    let args = Args::parse();

    let json = fs::read_to_string(&args.input)?;
    let input: DepositProofInput = serde_json::from_str(&json)?;
    let chain_id = input.resolve_chain_id(args.chain_id)?;
    println!("Proving a deposit on chain {chain_id}");
    let config = CircuitConfig {
        degree: args.degree,
        max_data_byte_len: args.max_data_byte_len,
        max_log_num: args.max_log_num,
        topic_num_bounds: (0, 4),
    };
    // Before the SRS and the key: a deposit the circuit cannot prove exits
    // with UNPROVABLE_EXIT_CODE, which callers do not retry.
    exit_if_unprovable(&input, &config);

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
    // Pin the keccak promise-loader capacity to FIXED_KECCAK_CAPACITY so this
    // proof verifies against the witness-independent VK that `export_vk_blob`
    // (and the live prover) produce.
    let preimages = keccak_shard::keccak_preimages(&input, &config);
    let circuit_input = DepositEventCircuitV2::new(input, &config);
    let fixed_keccak = PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY);
    let circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Prover,
        circuit_input,
        circuit_params,
        fixed_keccak,
    )
    .use_break_points(break_points);
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));

    let instances: Vec<Vec<Fr>> = circuit.instances();
    anyhow::ensure!(
        instances.len() == 1 && instances[0].len() == 12,
        "expected 1 instance column of 12 inputs, got {:?}",
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

    // The keccak table the deposit proof loaded, proven: its commitment is
    // public input 11.
    let commit = inst0[PROMISE_COMMIT_INDEX];
    anyhow::ensure!(
        keccak_shard::shard_commit(&preimages) == commit,
        "the keccak shard commitment differs from public input {PROMISE_COMMIT_INDEX}"
    );
    println!("Generating the keccak shard proof...");
    let shard_srs = if k == keccak_shard::KECCAK_SHARD_DEGREE {
        params
    } else {
        load_kzg_params_from_trusted_setup(keccak_shard::KECCAK_SHARD_DEGREE)
            .map_err(|e| anyhow::anyhow!("{e}"))?
    };
    let key = keccak_shard::keygen(&shard_srs)?;
    let keccak_proof = keccak_shard::prove(&shard_srs, &key, preimages, commit)?;
    anyhow::ensure!(
        keccak_shard::verify(&shard_srs, key.pk.get_vk(), commit, &keccak_proof),
        "verify_proof REJECTED the keccak shard proof"
    );
    println!(
        "  OK: keccak shard proof ({} bytes) VERIFIED.",
        keccak_proof.len()
    );

    fs::write(&args.proof_out, &proof)?;
    fs::write(&args.pubin_out, &pubin)?;
    fs::write(&args.keccak_proof_out, &keccak_proof)?;
    if let Some(path) = &args.keccak_vk_out {
        fs::write(path, keccak_shard::vk_blob(&key)?.to_bytes()?)?;
        println!("Wrote keccak VkBlob -> {path}");
    }
    println!(
        "Wrote proof -> {}\nWrote public inputs ({} bytes = {} Fr) -> {}\nWrote keccak proof -> {}",
        args.proof_out,
        pubin.len(),
        pubin.len() / 32,
        args.pubin_out,
        args.keccak_proof_out
    );
    println!(
        "\nRESULT: PASS — the deposit proof, its public inputs and the keccak proof are ready."
    );
    Ok(())
}
