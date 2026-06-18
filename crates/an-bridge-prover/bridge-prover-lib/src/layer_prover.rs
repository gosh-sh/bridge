//! Circuit 2 (Layer Historical Hashes Movement Checker) proof generation.

use anyhow::Context;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use rand::rngs::OsRng;
use tracing::info;

use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit,
    LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
};

use crate::keys::KeyManager;

/// Number of public instances Circuit 2 emits:
/// `block_id + bk_set_poseidon + num_layers + 10 layer hashes + prev_max_level_layer_hash = 14`.
pub const LAYER_HASHES_NUM_PUBLIC_INPUTS: usize = 1 + 1 + 1 + MAX_LAYERS + 1;

/// Bundled inputs for [`generate_layer_proof_with_input`]. Mirrors the
/// `LayerHashesProofInput<'a>` previously hand-rolled in
/// `bridge-prover-orchestrator::layer_hashes_prover` so the orchestrator can
/// drop its local copy and import this directly.
pub struct LayerHashesProofInput<'a> {
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: &'a [DenseChainLink],
    pub bk_set_poseidon_hash: Fr,
    /// Public-instance vector, computed off-circuit by the caller and asserted
    /// by the circuit. Order:
    /// `[block_id, bk_set_poseidon, num_layers, layer_hash_frs[0..10], prev_max_level_layer_hash]`.
    pub expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

/// Output type matching the orchestrator's `LayerHashesProofOutput` — proof
/// bytes plus the (caller-supplied) 14-element instance vector.
#[derive(Debug, Clone)]
pub struct LayerHashesProofOutput {
    pub proof_bytes: Vec<u8>,
    pub instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

/// Bundled-input wrapper around [`generate_layer_proof`]. Delegates to that
/// function then returns the proof together with the caller's pre-computed
/// instance vector, so binaries that already know the 14 Fr values do not
/// need to re-derive them from the rich [`LayerProofOutput`] fields.
pub fn generate_layer_proof_with_input(
    key_manager: &KeyManager,
    input: LayerHashesProofInput<'_>,
) -> anyhow::Result<LayerHashesProofOutput> {
    let out = generate_layer_proof(
        key_manager,
        &input.layer_hashes_preimage,
        &input.merkle_siblings,
        input.prev_max_level_layer_hash,
        input.num_prev_chain_steps,
        input.prev_chain_proofs,
        input.bk_set_poseidon_hash,
    )?;
    Ok(LayerHashesProofOutput {
        proof_bytes: out.proof_bytes,
        instances: input.expected_instances,
    })
}

/// Output of Circuit 2 proof generation.
#[derive(Debug, Clone)]
pub struct LayerProofOutput {
    pub proof_bytes: Vec<u8>,
    pub block_id_fr: Fr,
    pub bk_set_poseidon_hash_fr: Fr,
    pub num_layers: u8,
    pub layer_hash_frs: [Fr; 10],
    pub prev_max_level_layer_hash_fr: Fr,
}

/// Generate a Circuit 2 proof (layer historical hashes movement checker).
pub fn generate_layer_proof(
    key_manager: &KeyManager,
    layer_hashes_preimage: &[u8; LAYER_PREIMAGE_SIZE],
    merkle_siblings: &[[u8; 32]; 3],
    prev_max_level_layer_hash: Fr,
    num_prev_chain_steps: u8,
    prev_chain_proofs: &[DenseChainLink],
    bk_set_poseidon_hash: Fr,
) -> anyhow::Result<LayerProofOutput> {
    info!(
        "generating Circuit 2 proof: num_layers={}, chain_steps={}",
        layer_hashes_preimage[0], num_prev_chain_steps
    );

    // Extract expected public instances from the preimage.
    let num_layers = layer_hashes_preimage[0];
    let mut layer_hash_frs = [Fr::zero(); 10];
    for i in 0..MAX_LAYERS {
        let offset = 2 + i * 33;
        let hash_bytes = &layer_hashes_preimage[offset..offset + 32];
        let mut repr = [0u8; 32];
        repr.copy_from_slice(hash_bytes);
        layer_hash_frs[i] = Option::from(Fr::from_repr(repr)).unwrap_or(Fr::zero());
    }

    // Compute block_id Fr from the Merkle root.
    // This is computed by the circuit from SHA-256 Merkle path.
    // We compute it natively to set up the public instance.
    let block_id_fr = compute_block_id_fr_native(layer_hashes_preimage, merkle_siblings);

    // Build circuit.
    let k = key_manager.layer_k();
    let num_unusable = key_manager.layer_num_unusable_rows();
    let lookup_bits = key_manager.layer_lookup_bits();

    let mut circuit = LayerHashesMovementCheckerCircuit::new(
        *layer_hashes_preimage,
        *merkle_siblings,
        prev_max_level_layer_hash,
        num_prev_chain_steps,
        prev_chain_proofs.to_vec(),
        bk_set_poseidon_hash,
        k,
        num_unusable,
        lookup_bits,
    );
    circuit.override_base_circuit_params(key_manager.layer_config().clone());

    // Build public instances (14 values).
    let mut instances = Vec::with_capacity(14);
    instances.push(block_id_fr);             // [0]
    instances.push(bk_set_poseidon_hash);    // [1]
    instances.push(Fr::from(num_layers as u64)); // [2]
    for i in 0..MAX_LAYERS {
        instances.push(layer_hash_frs[i]);   // [3..12]
    }
    instances.push(prev_max_level_layer_hash); // [13]

    let instance_refs: &[&[Fr]] = &[&instances];
    let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
    create_proof::<
        KZGCommitmentScheme<Bn256>,
        ProverSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        _,
        Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
        _,
    >(
        &key_manager.srs,
        key_manager.layer_pk(),
        &[circuit],
        &[instance_refs],
        OsRng,
        &mut transcript,
    )
    .context("Circuit 2 proof generation failed")?;
    let proof_bytes = transcript.finalize();

    Ok(LayerProofOutput {
        proof_bytes,
        block_id_fr,
        bk_set_poseidon_hash_fr: bk_set_poseidon_hash,
        num_layers,
        layer_hash_frs,
        prev_max_level_layer_hash_fr: prev_max_level_layer_hash,
    })
}

/// Compute block_id Fr natively from layer_hashes_preimage + merkle_siblings.
///
/// Matches the in-circuit computation: Poseidon(preimage) → L0 bytes,
/// then SHA-256 Merkle path with 3 siblings → root, reverse → LE → Fr.
fn compute_block_id_fr_native(
    preimage: &[u8; LAYER_PREIMAGE_SIZE],
    siblings: &[[u8; 32]; 3],
) -> Fr {
    use sha2::{Digest, Sha256};

    // L0 = Poseidon(preimage chunks)
    let l0_hash = bridge_poseidon::poseidon_hash_bytes(preimage);
    let mut l0_bytes = [0u8; 32];
    l0_bytes.copy_from_slice(&l0_hash);

    // H_0 = SHA-256(L0_bytes || siblings[0])
    let mut h0_input = Vec::with_capacity(64);
    h0_input.extend_from_slice(&l0_bytes);
    h0_input.extend_from_slice(&siblings[0]);
    let h0: [u8; 32] = Sha256::digest(&h0_input).into();

    // H_01 = SHA-256(H_0 || siblings[1])
    let mut h01_input = Vec::with_capacity(64);
    h01_input.extend_from_slice(&h0);
    h01_input.extend_from_slice(&siblings[1]);
    let h01: [u8; 32] = Sha256::digest(&h01_input).into();

    // Root = SHA-256(H_01 || siblings[2])
    let mut root_input = Vec::with_capacity(64);
    root_input.extend_from_slice(&h01);
    root_input.extend_from_slice(&siblings[2]);
    let root_be: [u8; 32] = Sha256::digest(&root_input).into();

    // SHA-256 outputs big-endian. Convert to LE for Fr.
    let mut root_le = root_be;
    root_le.reverse();

    // Convert LE bytes to Fr via inner product with powers of 256.
    let mut result = Fr::zero();
    let mut power = Fr::one();
    let base = Fr::from(256u64);
    for &byte in &root_le {
        result += Fr::from(byte as u64) * power;
        power *= base;
    }
    result
}
