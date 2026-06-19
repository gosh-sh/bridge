//! Phase 1.B — Circuit 2 (Layer Hashes Movement) prover + native verifier.
//!
//! Produces Halo2 SHPLONK proofs for the partner's
//! [`LayerHashesMovementCheckerCircuit`] and verifies them off-chain. Public
//! instances (14 Fr elements):
//! ```text
//!   [0]      block_id                       (32-byte block-id hash, mod Fr)
//!   [1]      bk_set_poseidon                (commitment to the BK set)
//!   [2]      num_layers                     (u8 → Fr, 1..=10)
//!   [3..=12] layer_hash_frs[0..MAX_LAYERS]  (Fr per layer; 0 for inactive)
//!   [13]     prev_max_level_layer_hash      (Poseidon root of previous chain)
//! ```
//!
//! The instance order matches `build_layer_hashes_constraints`'s push sequence
//! in `historical-layer-hashes-movement-checker-circuit/src/circuit.rs:
//! 289-296`.

use anyhow::Context;
use gosh_dense_balanced_tree::DenseChainLink;
use halo2_base::halo2_proofs::{
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
};
use historical_layer_hashes_movement_checker_circuit::{
    circuit::LayerHashesMovementCheckerCircuit, LAYER_PREIMAGE_SIZE, MAX_LAYERS,
    NUM_MERKLE_SIBLINGS,
};
use rand::rngs::OsRng;
use tracing::info;

use crate::layer_hashes_keys::{
    LayerHashesKeyManager, LAYER_HASHES_K, LAYER_HASHES_LOOKUP_BITS, LAYER_HASHES_NUM_UNUSABLE_ROWS,
};

/// Number of public instances Circuit 2 emits (block_id + bk_set + num_layers
/// + 10 layer hashes + prev_max_level_layer_hash = 14).
pub const LAYER_HASHES_NUM_PUBLIC_INPUTS: usize = 1 + 1 + 1 + MAX_LAYERS + 1;

/// Inputs to a Circuit 2 proof.
pub struct LayerHashesProofInput<'a> {
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: &'a [DenseChainLink],
    pub bk_set_poseidon_hash: Fr,
    /// Public-instance vector, computed off-circuit by the caller and
    /// asserted by the circuit. Must have `LAYER_HASHES_NUM_PUBLIC_INPUTS`
    /// elements in the order documented at the module level.
    pub expected_instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

/// Output of a Circuit 2 proof generation.
#[derive(Debug, Clone)]
pub struct LayerHashesProofOutput {
    pub proof_bytes: Vec<u8>,
    pub instances: [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS],
}

impl LayerHashesProofOutput {
    pub fn instances(&self) -> [Fr; LAYER_HASHES_NUM_PUBLIC_INPUTS] {
        self.instances
    }
}

/// Generate a Circuit 2 proof (Blake2b transcript — AN-side default).
pub fn generate_layer_hashes_proof(
    key_manager: &LayerHashesKeyManager,
    input: LayerHashesProofInput<'_>,
) -> anyhow::Result<LayerHashesProofOutput> {
    generate_layer_hashes_proof_with_transcript(
        key_manager,
        input,
        crate::halo2_tvm_bundle::TranscriptKind::Blake2b,
    )
}

/// Generate a Circuit 2 proof with the chosen Fiat–Shamir transcript.
pub fn generate_layer_hashes_proof_with_transcript(
    key_manager: &LayerHashesKeyManager,
    input: LayerHashesProofInput<'_>,
    transcript: crate::halo2_tvm_bundle::TranscriptKind,
) -> anyhow::Result<LayerHashesProofOutput> {
    use crate::halo2_tvm_bundle::TranscriptKind;
    use crate::poseidon_transcript::PoseidonWrite;

    info!(
        num_chain_steps = input.num_prev_chain_steps,
        chain_links = input.prev_chain_proofs.len(),
        ?transcript,
        "generating layer-hashes proof"
    );

    let mut circuit = LayerHashesMovementCheckerCircuit::new(
        input.layer_hashes_preimage,
        input.merkle_siblings,
        input.prev_max_level_layer_hash,
        input.num_prev_chain_steps,
        input.prev_chain_proofs.to_vec(),
        input.bk_set_poseidon_hash,
        LAYER_HASHES_K as usize,
        LAYER_HASHES_NUM_UNUSABLE_ROWS,
        LAYER_HASHES_LOOKUP_BITS,
    );
    circuit.override_base_circuit_params(key_manager.config().clone());

    let instance_refs: &[&[Fr]] = &[&input.expected_instances];

    let proof_bytes = match transcript {
        TranscriptKind::Blake2b => {
            let mut t = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                Challenge255<G1Affine>,
                _,
                Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
                _,
            >(
                &key_manager.srs,
                key_manager.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("layer-hashes proof generation failed (Blake2b transcript)")?;
            t.finalize()
        },
        TranscriptKind::Poseidon => {
            let mut t = PoseidonWrite::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                _,
                _,
                PoseidonWrite<Vec<u8>>,
                _,
            >(
                &key_manager.srs,
                key_manager.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("layer-hashes proof generation failed (Poseidon transcript)")?;
            t.finalize()
        },
    };

    Ok(LayerHashesProofOutput {
        proof_bytes,
        instances: input.expected_instances,
    })
}

/// Native (off-chain) Halo2 SHPLONK verification for a Circuit 2 proof.
pub fn verify_layer_hashes_proof(
    key_manager: &LayerHashesKeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    let instance_refs: &[&[Fr]] = &[instances];
    let verifier_params = key_manager.srs.verifier_params();
    let strategy = SingleStrategy::new(&key_manager.srs);
    let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(proof_bytes);
    verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
        SingleStrategy<'_, Bn256>,
    >(
        verifier_params,
        key_manager.vk(),
        strategy,
        &[instance_refs],
        &mut transcript,
    )
    .is_ok()
}
