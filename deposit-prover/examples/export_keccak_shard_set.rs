//! Export the keccak shard proofs for a deposit proof set: one shared shard
//! `VkBlob` and, next to each deposit proof, the proof of its keccak table.
//!
//! The deposit proofs are left alone. For each `proof_NN` the exporter takes
//! the keccak preimages from `input.json`, checks that their shard commitment
//! equals public input 11 in `public_inputs.bin`, proves it and verifies the
//! proof against the shared key.
//!
//! Layout written under `--set-dir`:
//!   <set-dir>/keccak_vk_blob.bin          (shared, v2 KeccakShard VkBlob)
//!   <set-dir>/proof_NN/keccak_proof.bin   (Blake2b SHPLONK proof)
//!
//! Run from `deposit-prover/` with `data/kzg_params_18.srs`:
//!   cargo run --release --example export_keccak_shard_set -- \
//!     --set-dir fixtures/deposit_10proofs --count 10

use std::{fs, path::Path};

use clap::Parser;
use deposit_prover::{
    halo2_tvm_bundle::{decode_instances, VkBlob},
    keccak_shard::{self, PROMISE_COMMIT_INDEX},
    load_kzg_params_from_trusted_setup,
    prover::CircuitConfig,
    types::{DepositProofInput, NUM_PUBLIC_INPUTS},
};

#[derive(Parser, Debug)]
#[command(name = "export-keccak-shard-set")]
struct Args {
    /// Directory holding `proof_NN/input.json` and
    /// `proof_NN/public_inputs.bin`.
    #[arg(long)]
    set_dir: String,
    /// Number of proofs (`proof_00`..`proof_{count-1}`).
    #[arg(long, default_value = "10")]
    count: usize,
    /// First proof to write; the earlier ones are left as they are.
    #[arg(long, default_value = "0")]
    first: usize,
    /// Read the existing `keccak_vk_blob.bin` and check the proofs. Do not
    /// generate a key or write anything.
    #[arg(long)]
    check_only: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = CircuitConfig::production();
    let srs = load_kzg_params_from_trusted_setup(keccak_shard::KECCAK_SHARD_DEGREE)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let (key, vk) = if args.check_only {
        let blob = fs::read(format!("{}/keccak_vk_blob.bin", args.set_dir))?;
        let vk = keccak_shard::read_vk(&VkBlob::read(blob.as_slice())?)?;
        println!("  read keccak_vk_blob.bin ({} bytes)", blob.len());
        (None, vk)
    } else {
        println!("Shard keygen ...");
        let key = keccak_shard::keygen(&srs)?;
        let blob = keccak_shard::vk_blob(&key)?.to_bytes()?;
        let vk = keccak_shard::read_vk(&VkBlob::read(blob.as_slice())?)?;
        fs::write(format!("{}/keccak_vk_blob.bin", args.set_dir), &blob)?;
        println!("  wrote keccak_vk_blob.bin ({} bytes)", blob.len());
        (Some(key), vk)
    };

    for i in args.first..args.count {
        let dir = format!("{}/proof_{i:02}", args.set_dir);
        if !Path::new(&format!("{dir}/input.json")).exists() {
            println!("proof_{i:02}: SKIP (no input.json)");
            continue;
        }
        let public_inputs = decode_instances(&fs::read(format!("{dir}/public_inputs.bin"))?)?;
        anyhow::ensure!(
            public_inputs.len() == NUM_PUBLIC_INPUTS,
            "proof_{i:02}: public_inputs.bin holds {} inputs, not {NUM_PUBLIC_INPUTS}",
            public_inputs.len()
        );
        let commit = public_inputs[PROMISE_COMMIT_INDEX];

        let proof = if args.check_only {
            fs::read(format!("{dir}/keccak_proof.bin"))?
        } else {
            let input: DepositProofInput =
                serde_json::from_str(&fs::read_to_string(format!("{dir}/input.json"))?)?;
            let preimages = keccak_shard::keccak_preimages(&input, &config);
            anyhow::ensure!(
                keccak_shard::shard_commit(&preimages) == commit,
                "proof_{i:02}: the shard commitment of input.json differs from public input \
                 {PROMISE_COMMIT_INDEX} of public_inputs.bin"
            );
            let proof = keccak_shard::prove(&srs, key.as_ref().unwrap(), preimages, commit)?;
            fs::write(format!("{dir}/keccak_proof.bin"), &proof)?;
            proof
        };
        anyhow::ensure!(
            keccak_shard::verify(&srs, &vk, commit, &proof),
            "proof_{i:02}: the shard proof does not verify against keccak_vk_blob.bin"
        );
        println!("proof_{i:02}: keccak_proof={}B -> VERIFIED", proof.len());
    }
    Ok(())
}
