//! The keccak proof that goes with every deposit proof.
//!
//! The deposit circuit does not compute keccak itself. It loads a table of
//! `(preimage, hash)` rows as witness, checks each of its hashes against that
//! table, and exposes a Poseidon commitment to the table as public input 11.
//! The deposit proof alone says nothing about whether the rows are real
//! keccak results: a prover can load a table with made-up hashes and still
//! produce a valid proof.
//!
//! This module proves the table. [`KeccakComponentShardCircuit`] hashes the
//! same preimages, in the same order and padded to the same capacity, and
//! publishes the commitment to its outputs as its only public input. A
//! verifier that accepts both proofs and finds the two commitments equal
//! knows every hash the deposit circuit relied on is genuine.

use axiom_eth::{
    keccak::types::{ComponentTypeKeccak, KeccakLogicalInput},
    mpt::MPTChip,
    rlp::RlpChip,
    utils::{
        component::{
            promise_loader::single::PromiseLoaderParams, utils::try_from_key, ComponentType,
        },
        eth_circuit::{EthCircuitImpl, EthCircuitInstructions},
    },
};
use halo2_base::{
    gates::{circuit::CircuitBuilderStage, flex_gate::MultiPhaseThreadBreakPoints},
    halo2_proofs::{
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::{create_proof, keygen_pk, keygen_vk, verify_proof, ProvingKey, VerifyingKey},
        poly::{
            commitment::ParamsProver,
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
use zkevm_hashes::keccak::{
    component::{
        circuit::shard::{KeccakComponentShardCircuit, KeccakComponentShardCircuitParams},
        output::{calculate_circuit_outputs_commit, multi_inputs_to_circuit_outputs},
    },
    vanilla::keccak_packed_multi::get_num_keccak_f,
};

use crate::{
    circuit_v2::DepositEventCircuitV2,
    halo2_tvm_bundle::VkBlob,
    prover::{get_default_params, CircuitConfig, FIXED_KECCAK_CAPACITY},
    types::DepositProofInput,
};

/// Degree of the keccak shard circuit. The same as the deposit circuit's, so
/// both proofs use `data/kzg_params_18.srs`. Part of the shard verifying key.
pub const KECCAK_SHARD_DEGREE: u32 = 18;

/// Rows halo2 reserves at the bottom of the table.
const UNUSABLE_ROWS: usize = 109;

/// Index of the deposit public input the shard commitment has to equal.
pub const PROMISE_COMMIT_INDEX: usize = 11;

/// The shard circuit's parameters: [`KECCAK_SHARD_DEGREE`] rows, capacity
/// [`FIXED_KECCAK_CAPACITY`] permutations — the deposit circuit pads its
/// table to that capacity, and the commitment covers the padding — and the
/// outputs published as one Poseidon commitment.
pub fn shard_params() -> KeccakComponentShardCircuitParams {
    let mut params = KeccakComponentShardCircuitParams::new(
        KECCAK_SHARD_DEGREE as usize,
        UNUSABLE_ROWS,
        FIXED_KECCAK_CAPACITY,
        false,
    );
    params.base_circuit_params =
        KeccakComponentShardCircuit::<Fr>::calculate_base_circuit_params(&params);
    params
}

/// The keccak preimages the deposit circuit asks for on `input`, in the order
/// its table holds them.
pub fn keccak_preimages(input: &DepositProofInput, config: &CircuitConfig) -> Vec<Vec<u8>> {
    let circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Mock,
        DepositEventCircuitV2::new(input.clone(), config),
        get_default_params(),
        PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY),
    );
    let rlp = RlpChip::new(circuit.keccak_chip.range(), None);
    let mpt = MPTChip::new(rlp, &circuit.keccak_chip);
    DepositEventCircuitV2::virtual_assign_phase0(
        &circuit.logic_inputs,
        &mut circuit.rlc_builder.borrow_mut(),
        &mpt,
    );
    let calls = circuit
        .promise_collector
        .lock()
        .unwrap()
        .get_deduped_calls();
    let preimages = calls
        .get(&ComponentTypeKeccak::<Fr>::get_type_id())
        .map(|calls| {
            calls
                .iter()
                .map(|c| {
                    try_from_key::<KeccakLogicalInput>(&c.logical_input)
                        .expect("a keccak promise call carries a keccak input")
                        .bytes
                })
                .collect()
        })
        .unwrap_or_default();
    circuit.clear_witnesses();
    preimages
}

/// Keccak permutations `preimages` take up.
pub fn used_capacity(preimages: &[Vec<u8>]) -> usize {
    preimages.iter().map(|p| get_num_keccak_f(p.len())).sum()
}

/// The commitment the shard circuit publishes for `preimages` — and the
/// deposit circuit's public input 11 for the same calls.
pub fn shard_commit(preimages: &[Vec<u8>]) -> Fr {
    calculate_circuit_outputs_commit(&multi_inputs_to_circuit_outputs::<Fr>(
        preimages,
        FIXED_KECCAK_CAPACITY,
    ))
}

/// Proving key of the shard circuit with the break points its prover needs.
/// The circuit's shape does not depend on the preimages, so one key serves
/// every deposit.
pub struct ShardKey {
    pub pk: ProvingKey<G1Affine>,
    pub break_points: MultiPhaseThreadBreakPoints,
    pub params: KeccakComponentShardCircuitParams,
}

/// Generates the shard proving key on `srs` (degree [`KECCAK_SHARD_DEGREE`]).
pub fn keygen(srs: &ParamsKZG<Bn256>) -> anyhow::Result<ShardKey> {
    let params = shard_params();
    let circuit = KeccakComponentShardCircuit::<Fr>::new(vec![], params.clone(), false);
    let vk = keygen_vk(srs, &circuit).map_err(|e| anyhow::anyhow!("shard keygen_vk: {e:?}"))?;
    let pk = keygen_pk(srs, vk, &circuit).map_err(|e| anyhow::anyhow!("shard keygen_pk: {e:?}"))?;
    Ok(ShardKey {
        pk,
        break_points: circuit.base_circuit_break_points(),
        params,
    })
}

/// A Blake2b-transcript SHPLONK proof that the keccak outputs of `preimages`
/// commit to `commit` — the `proof_cell` operand of `ZKHALO2VERIFYWITHVK`.
pub fn prove(
    srs: &ParamsKZG<Bn256>,
    key: &ShardKey,
    preimages: Vec<Vec<u8>>,
    commit: Fr,
) -> anyhow::Result<Vec<u8>> {
    let used = used_capacity(&preimages);
    anyhow::ensure!(
        used <= FIXED_KECCAK_CAPACITY,
        "the preimages need {used} keccak permutations, over the shard's capacity of \
         {FIXED_KECCAK_CAPACITY}"
    );
    let circuit = KeccakComponentShardCircuit::<Fr>::new(preimages, key.params.clone(), true);
    circuit.set_base_circuit_break_points(key.break_points.clone());
    let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(Vec::new());
    create_proof::<KZGCommitmentScheme<Bn256>, ProverSHPLONK<'_, Bn256>, _, _, _, _>(
        srs,
        &key.pk,
        &[circuit],
        &[&[&[commit]]],
        OsRng,
        &mut transcript,
    )
    .map_err(|e| anyhow::anyhow!("shard create_proof: {e:?}"))?;
    Ok(transcript.finalize())
}

/// The check `ZKHALO2VERIFYWITHVK` runs on a shard proof.
pub fn verify(
    srs: &ParamsKZG<Bn256>,
    vk: &VerifyingKey<G1Affine>,
    commit: Fr,
    proof: &[u8],
) -> bool {
    let mut transcript = Blake2bRead::<_, G1Affine, Challenge255<_>>::init(proof);
    verify_proof::<_, VerifierSHPLONK<'_, Bn256>, _, _, SingleStrategy<'_, Bn256>>(
        srs.verifier_params(),
        vk,
        SingleStrategy::new(srs),
        &[&[&[commit]]],
        &mut transcript,
    )
    .is_ok()
}

/// The shard verifying key as a `VkBlob` — the `vk_cell` operand.
pub fn vk_blob(key: &ShardKey) -> anyhow::Result<VkBlob> {
    let json = serde_json::to_vec(&key.params)?;
    VkBlob::from_native_keccak_shard(json, key.pk.get_vk())
}

/// Reads a shard verifying key back from a `VkBlob` the way the opcode does.
pub fn read_vk(blob: &VkBlob) -> anyhow::Result<VerifyingKey<G1Affine>> {
    use halo2_base::halo2_proofs::SerdeFormat;
    let crate::halo2_tvm_bundle::VkConfig::KeccakShard(json) = &blob.config else {
        anyhow::bail!("the VkBlob is not a keccak shard key");
    };
    let params: KeccakComponentShardCircuitParams = serde_json::from_slice(json)?;
    Ok(VerifyingKey::<G1Affine>::read::<
        _,
        KeccakComponentShardCircuit<Fr>,
    >(
        &mut blob.vk_bytes.as_slice(),
        SerdeFormat::RawBytes,
        params,
    )?)
}
