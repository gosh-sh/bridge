//! Conclusive in-process reproduction of the AN `ZKHALO2VERIFYWITHVK` opcode
//! handler, run on the EXACT on-wire artifacts produced by `export_vk_blob`
//! and `export_blake2b_proof`:
//!
//!   1. Read the `vk_cell` payload (`deposit_vk_blob.bin`) → `VkBlob::read`.
//!   2. Parse the carried `EthCircuitParams` and `VerifyingKey::read` it via
//!      `EthCircuitImpl<Fr, Noop>` (the opcode's RLC branch).
//!   3. Read the `public_inputs_cell` payload (strict `N × 32` LE `Fr`).
//!   4. Read the `proof_cell` payload (raw Blake2b SHPLONK bytes).
//!   5. `verify_proof::<KZG, VerifierSHPLONK, _, Blake2bRead, SingleStrategy>`
//!      against `srs.verifier_params()`.
//!
//! A PASS here means: the deposit VK serialised into the v2 RLC blob, the
//! Blake2b proof, and the 12 public inputs form a self-consistent triple that
//! the AN node opcode will accept — using only the bytes that cross the wire.
//!
//! Usage:
//!   cargo run --release --example verify_opcode_triple -- \
//!     --vk-blob   /tmp/deposit_e2e/deposit_vk_blob.bin \
//!     --proof     /tmp/deposit_e2e/deposit_proof_blake2b.bin \
//!     --pubin     /tmp/deposit_e2e/deposit_public_inputs.bin \
//!     --degree 18

use std::fs;

use axiom_eth::{
    mpt::MPTChip,
    rlc::circuit::builder::RlcCircuitBuilder,
    utils::eth_circuit::{EthCircuitImpl, EthCircuitInstructions, EthCircuitParams},
    Field,
};
use clap::Parser;
use deposit_prover::prover::load_kzg_params_from_trusted_setup;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{verify_proof, VerifyingKey},
    poly::{
        commitment::{Params, ParamsProver},
        kzg::{
            commitment::{KZGCommitmentScheme, ParamsKZG},
            multiopen::VerifierSHPLONK,
            strategy::SingleStrategy,
        },
    },
    transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer},
    SerdeFormat,
};
use deposit_prover::halo2_tvm_bundle::{decode_instances, CircuitShape, VkBlob, VkConfig};

#[derive(Clone)]
struct Noop;
impl<F: Field> EthCircuitInstructions<F> for Noop {
    type FirstPhasePayload = ();
    fn virtual_assign_phase0(&self, builder: &mut RlcCircuitBuilder<F>, mpt: &MPTChip<F>) {
        let keccak = mpt.keccak();
        let ctx = builder.base.main(0);
        let b = ctx.load_witness(F::ZERO);
        let _ = keccak.keccak_fixed_len(ctx, vec![b]);
    }
}

#[derive(Parser, Debug)]
#[command(name = "verify-opcode-triple")]
struct Args {
    #[arg(long)]
    vk_blob: String,
    #[arg(long)]
    proof: String,
    #[arg(long)]
    pubin: String,
    #[arg(long, default_value = "18")]
    degree: u32,
}

fn main() -> anyhow::Result<()> {
    println!("=== Reproduce ZKHALO2VERIFYWITHVK handler on on-wire artifacts ===\n");
    let args = Args::parse();

    // 1+2. vk_cell -> VkBlob -> EthCircuitParams -> VerifyingKey (RLC branch).
    let blob_bytes = fs::read(&args.vk_blob)?;
    let blob = VkBlob::read(blob_bytes.as_slice())?;
    anyhow::ensure!(
        blob.shape() == CircuitShape::Rlc,
        "vk_blob is not RLC-shape"
    );
    let eth_json = match &blob.config {
        VkConfig::Rlc(j) => j.clone(),
        VkConfig::Base(_) => anyhow::bail!("vk_blob carries a Base config; expected Rlc"),
    };
    let eth_params: EthCircuitParams = serde_json::from_slice(&eth_json)?;
    let mut vk_slice = blob.vk_bytes.as_slice();
    let vk = VerifyingKey::<G1Affine>::read::<_, EthCircuitImpl<Fr, Noop>>(
        &mut vk_slice,
        SerdeFormat::RawBytes,
        eth_params,
    )
    .map_err(|e| anyhow::anyhow!("opcode RLC VerifyingKey::read failed: {e}"))?;
    println!("[1] VkBlob read + VK reconstructed via EthCircuitImpl<Fr, Noop>.");

    // 3. public_inputs_cell -> strict Fr.
    let pubin = fs::read(&args.pubin)?;
    let instances = decode_instances(&pubin)?;
    anyhow::ensure!(
        instances.len() == 12,
        "expected 12 public inputs, got {}",
        instances.len()
    );
    println!(
        "[2] {} public inputs decoded (strict 32-byte LE).",
        instances.len()
    );

    // 4. proof_cell.
    let proof = fs::read(&args.proof)?;
    println!("[3] proof: {} bytes.", proof.len());

    // 5. verify_proof — the opcode's core check.
    let srs = load_kzg_params_from_trusted_setup(args.degree).map_err(|e| anyhow::anyhow!("{e}"))?;

    let strategy = SingleStrategy::new(&srs);
    let mut transcript = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(proof.as_slice());
    verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
        SingleStrategy<'_, Bn256>,
    >(
        srs.verifier_params(),
        &vk,
        strategy,
        &[&[instances.as_slice()]],
        &mut transcript,
    )
    .map_err(|e| anyhow::anyhow!("verify_proof REJECTED the triple: {e:?}"))?;

    println!("[4] verify_proof ACCEPTED.\n");
    println!("Public inputs (finalizeDeposit layout):");
    let names = [
        "depositId",
        "sender",
        "amount",
        "contractAddress",
        "blockHashHigh",
        "blockHashLow",
        "promiseCommit",
    ];
    for (n, fr) in names.iter().zip(instances.iter()) {
        println!("  {n:>16} = {fr:?}");
    }
    println!("\nRESULT: PASS — AN opcode will accept (vk_blob, public_inputs, proof).");
    Ok(())
}
