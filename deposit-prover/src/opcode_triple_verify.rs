//! AN `ZKHALO2VERIFYWITHVK` opcode triple verification (Blake2b + VerifierSHPLONK).
//!
//! Deposit VkBlobs are RLC-shaped; [`crate::halo2_tvm_bundle::Halo2TvmOperands::verify`]
//! only round-trips Base blobs. This module mirrors `examples/verify_opcode_triple.rs`.

use std::path::Path;

use axiom_eth::{
    mpt::MPTChip,
    rlc::circuit::builder::RlcCircuitBuilder,
    utils::{
        component::promise_loader::single::PromiseLoaderParams,
        eth_circuit::{EthCircuitImpl, EthCircuitInstructions, EthCircuitParams},
    },
    Field,
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{create_proof, verify_proof, VerifyingKey},
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
        SerdeFormat,
    },
};
use rand::rngs::OsRng;

use crate::{
    circuit_v2::DepositEventCircuitV2,
    halo2_tvm_bundle::{decode_instances, encode_instances, CircuitShape, VkBlob, VkConfig},
    prover::{
        get_or_create_proving_key, load_kzg_params_from_trusted_setup, CircuitConfig,
        FIXED_KECCAK_CAPACITY,
    },
    types::DepositProofInput,
};

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

/// Verify on-wire `(vk_blob, public_inputs, proof)` like the AN opcode handler.
pub fn verify_deposit_opcode_triple(
    vk_blob: &[u8],
    proof: &[u8],
    public_inputs: &[u8],
    degree: u32,
) -> Result<(), String> {
    let blob = VkBlob::read(vk_blob).map_err(|e| format!("VkBlob::read: {e}"))?;
    if blob.shape() != CircuitShape::Rlc {
        return Err("vk_blob is not RLC-shape (deposit circuit)".to_string());
    }
    let eth_json = match &blob.config {
        VkConfig::Rlc(j) => j.clone(),
        VkConfig::Base(_) => return Err("vk_blob carries Base config; expected Rlc".to_string()),
    };
    let eth_params: EthCircuitParams =
        serde_json::from_slice(&eth_json).map_err(|e| format!("EthCircuitParams JSON: {e}"))?;
    let mut vk_slice = blob.vk_bytes.as_slice();
    let vk = VerifyingKey::<G1Affine>::read::<_, EthCircuitImpl<Fr, Noop>>(
        &mut vk_slice,
        SerdeFormat::RawBytes,
        eth_params,
    )
    .map_err(|e| format!("VerifyingKey::read: {e}"))?;

    let instances = decode_instances(public_inputs).map_err(|e| format!("decode_instances: {e}"))?;
    if instances.len() != 12 {
        return Err(format!(
            "expected 12 public inputs, got {}",
            instances.len()
        ));
    }

    let srs = load_kzg_params_from_trusted_setup(degree)
        .map_err(|e| format!("load_kzg_params_from_trusted_setup: {e}"))?;
    let strategy = SingleStrategy::new(&srs);
    let mut transcript = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(proof);
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
    .map_err(|e| format!("SHPLONK verify_proof rejected triple: {e:?}"))?;
    Ok(())
}

/// Export Blake2b-transcript SHPLONK proof + strict public-input bytes for opcode tests.
pub fn export_blake2b_deposit_triple(
    input: DepositProofInput,
    config: &CircuitConfig,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    input
        .resolve_chain_id(None)
        .map_err(|e| format!("resolve_chain_id: {e}"))?;
    let k = config.degree;
    let params = load_kzg_params_from_trusted_setup(k)
        .map_err(|e| format!("load_kzg_params_from_trusted_setup: {e}"))?;

    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    std::fs::create_dir_all("data").map_err(|e| format!("create data dir: {e}"))?;
    let (pk, circuit_params, break_points) = get_or_create_proving_key(
        &params,
        &input,
        config,
        Path::new(&pk_path_str),
    )
    .map_err(|e| format!("get_or_create_proving_key: {e}"))?;

    let circuit_input = DepositEventCircuitV2::new(input, config);
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
    if instances.len() != 1 || instances[0].len() != 12 {
        return Err(format!(
            "expected 1×12 instances, got {:?}",
            instances.iter().map(|c| c.len()).collect::<Vec<_>>()
        ));
    }
    let inst0 = instances[0].clone();

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
    .map_err(|e| format!("create_proof failed: {e:?}"))?;
    let proof = transcript.finalize();
    let pubin = encode_instances(&inst0);
    Ok((proof, pubin))
}
