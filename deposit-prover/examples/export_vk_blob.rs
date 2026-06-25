//! Export the deposit circuit's verifying key as a **v2 RLC `VkBlob`** — the
//! wire the AN-side `ZKHALO2VERIFYWITHVK` opcode reads to verify a deposit
//! proof natively (see `docs/deposit_finalize_vk_gap_2026-05-28.md`).
//!
//! Pipeline:
//!   1. Rebuild the deposit keygen circuit (`EthCircuitImpl<Fr,
//!      DepositEventCircuitV2>`) from the SAME real input + config used to
//!      generate the proof, so the VK is bit-identical to the one the proof
//!      verifies against.
//!   2. `calculate_params()` → the canonical `EthCircuitParams` the opcode
//!      needs to reconstruct the constraint system on `VerifyingKey::read`.
//!   3. `keygen_vk` → the deposit `VerifyingKey<G1Affine>`.
//!   4. `VkBlob::from_native_rlc(EthCircuitParams JSON, vk)` → the v2 blob
//!      (magic `VKBLOB\0\0`, version 2, `circuit_shape = Rlc`).
//!   5. Round-trip self-check: read the blob back and `VerifyingKey::read` it
//!      via `EthCircuitImpl<Fr, Noop>` + the carried `EthCircuitParams` — the
//!      EXACT generic read the opcode's RLC branch performs (mirrors
//!      `vk-compat-check/rlc-reader`). A passing self-check means the AN node
//!      can deserialise this blob.
//!
//! Usage:
//!   cargo run --release --example export_vk_blob -- \
//!     --input /tmp/deposit_e2e/deposit_proof_input.json \
//!     --output /tmp/deposit_e2e/deposit_vk_blob.bin \
//!     --degree 18 --max-data-byte-len 256 --max-log-num 20

// Reuse the REAL producer-side wire format (the same module the orchestrator
// and the `vkblob-v2` isolation crate compile) so the bytes can never drift
// from what the opcode expects.
#[path = "../../crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs"]
mod halo2_tvm_bundle;

use std::fs;

use axiom_eth::{
    mpt::MPTChip,
    rlc::circuit::builder::RlcCircuitBuilder,
    utils::{
        component::promise_loader::single::PromiseLoaderParams,
        eth_circuit::{create_circuit, EthCircuitImpl, EthCircuitInstructions, EthCircuitParams},
    },
    Field,
};
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
        plonk::{keygen_vk, VerifyingKey},
        poly::{commitment::Params, kzg::commitment::ParamsKZG},
        SerdeFormat,
    },
};
use halo2_tvm_bundle::{CircuitShape, VkBlob};

/// Pinned keccak promise-loader capacity — makes the deposit VK
/// witness-independent (one embedded VK verifies every deposit). MUST match the
/// value used by the prover (`export_deposit_proof_set.rs` / `prover.rs`).
const FIXED_KECCAK_CAPACITY: usize = 64;

/// Minimal RLC + keccak shape stand-in — identical to the opcode-side reader's
/// `Noop`. `EthCircuitImpl::configure_with_params` is generic over the inner
/// instructions, so reading a `DepositEventCircuitV2`-shaped VK back through
/// `EthCircuitImpl<Fr, Noop>` is exactly what the node does.
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
#[command(name = "export-vk-blob")]
#[command(about = "Export the deposit VK as a v2 RLC VkBlob for ZKHALO2VERIFYWITHVK", long_about = None)]
struct Args {
    /// Input file containing the DepositProofInput (JSON) used for proving.
    #[arg(long)]
    input: String,

    /// Output path for the serialised VkBlob bytes.
    #[arg(long, default_value = "deposit_vk_blob.bin")]
    output: String,

    /// Path for the EthCircuitParams JSON (also embedded in the blob).
    #[arg(long, default_value = "deposit_eth_circuit_params.json")]
    config_out: String,

    /// Circuit degree (must match the proving run).
    #[arg(long, default_value = "18")]
    degree: u32,

    /// Max data byte length (must match the proving run).
    #[arg(long, default_value = "256")]
    max_data_byte_len: usize,

    /// Max log number (must match the proving run).
    #[arg(long, default_value = "20")]
    max_log_num: usize,
}

fn main() -> anyhow::Result<()> {
    println!("=== Export deposit VK as v2 RLC VkBlob ===\n");
    let args = Args::parse();

    println!("Loading input from {}...", args.input);
    let json = fs::read_to_string(&args.input)?;
    let input: DepositProofInput = serde_json::from_str(&json)?;

    let config = CircuitConfig {
        degree: args.degree,
        max_data_byte_len: args.max_data_byte_len,
        max_log_num: args.max_log_num,
        topic_num_bounds: (0, 4),
    };
    println!(
        "Config: degree={} max_data_byte_len={} max_log_num={}\n",
        config.degree, config.max_data_byte_len, config.max_log_num
    );

    // 1. Rebuild the keygen circuit exactly as the prover does.
    // Pin the keccak promise-loader capacity so the VK is witness-INDEPENDENT
    // (one embedded VK verifies every real deposit regardless of MPT proof
    // depth). Together with dropping the `contract_address` in-circuit constant
    // (circuit_v2.rs) this makes the VK fully witness-independent — no axiom-eth
    // fork change is needed. See `docs/deposit_vk_witness_independence.md`.
    let fixed_keccak = PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY);
    let circuit_input = DepositEventCircuitV2::new(input, &config);
    let rlc_params = get_default_params();
    let mut circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Keygen,
        circuit_input,
        rlc_params,
        fixed_keccak,
    );
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));

    // 2. Canonical EthCircuitParams (what the opcode needs to read the VK back).
    let eth_params: EthCircuitParams = circuit.calculate_params();
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));
    let k = eth_params.rlc.base.k as u32;
    println!(
        "Calculated EthCircuitParams: k={} num_rlc_columns={}",
        k, eth_params.rlc.num_rlc_columns
    );

    // 3. Load the SRS and keygen the VK (`data/kzg_params_{k}.srs` from
    // `download_trusted_setup.sh`).
    let srs = load_kzg_params_from_trusted_setup(k)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Running keygen_vk (this may take a minute)...");
    let vk = keygen_vk(&srs, &circuit).map_err(|e| anyhow::anyhow!("keygen_vk failed: {e:?}"))?;
    println!("VK generated.\n");

    // 4. Build the v2 RLC VkBlob via the real producer-side path.
    let eth_config_json = serde_json::to_vec(&eth_params)?;
    let blob = VkBlob::from_native_rlc(eth_config_json.clone(), &vk)?;
    assert_eq!(blob.shape(), CircuitShape::Rlc, "must be RLC shape");
    assert_eq!(blob.version(), 2, "RLC shape serialises as v2");

    let blob_bytes = blob.to_bytes()?;
    fs::write(&args.output, &blob_bytes)?;
    fs::write(&args.config_out, &eth_config_json)?;
    println!(
        "Wrote VkBlob ({} bytes) -> {}\nWrote EthCircuitParams JSON -> {}\n",
        blob_bytes.len(),
        args.output,
        args.config_out
    );

    // 5. Round-trip self-check: exactly the opcode's RLC read branch.
    println!("Round-trip self-check (opcode RLC read path)...");
    let back = VkBlob::read(blob_bytes.as_slice())?;
    assert_eq!(back.shape(), CircuitShape::Rlc);
    let carried_json = match &back.config {
        halo2_tvm_bundle::VkConfig::Rlc(j) => j.clone(),
        halo2_tvm_bundle::VkConfig::Base(_) => {
            anyhow::bail!("read back a Base config; expected Rlc")
        },
    };
    let parsed_params: EthCircuitParams = serde_json::from_slice(&carried_json)
        .map_err(|e| anyhow::anyhow!("parse carried EthCircuitParams: {e}"))?;

    let mut slice = back.vk_bytes.as_slice();
    let _reread: VerifyingKey<G1Affine> = VerifyingKey::<G1Affine>::read::<
        _,
        EthCircuitImpl<Fr, Noop>,
    >(&mut slice, SerdeFormat::RawBytes, parsed_params)
    .map_err(|e| anyhow::anyhow!("opcode-path VerifyingKey::read failed: {e}"))?;
    println!("  OK: VK reads back via EthCircuitImpl<Fr, Noop> + carried EthCircuitParams.");

    println!("\nRESULT: PASS — deposit v2 RLC VkBlob is opcode-readable.");
    Ok(())
}
