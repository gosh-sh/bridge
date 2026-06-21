//! Circuit 4 (bridge-event-prove-circuit) proof generation with transcript choice.
//!
//! Uses partner `bridge-event-prove-circuit` + `bridge_prover_lib::KeyManager::ensure_event_keys`.

use anyhow::Context;
use bridge_event_prove_circuit::bridge_event_prove_circuit::BridgeEventProveCircuit;
use bridge_prover_lib::keys::KeyManager;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::info;

use crate::{halo2_tvm_bundle::TranscriptKind, poseidon_transcript::PoseidonWrite};

/// Number of public instances Circuit 4 emits (single-final-root layout).
pub const CIRCUIT4_NUM_PUBLIC_INPUTS: usize = 10;

/// Output of a Circuit 4 proof generation.
#[derive(Debug, Clone)]
pub struct Circuit4ProofOutput {
    pub proof_bytes: Vec<u8>,
    pub instances: [Fr; CIRCUIT4_NUM_PUBLIC_INPUTS],
}

/// Generate a Circuit 4 proof (Blake2b transcript).
pub fn generate_circuit4_proof(
    key_manager: &mut KeyManager,
    circuit: BridgeEventProveCircuit,
    instances: [Fr; CIRCUIT4_NUM_PUBLIC_INPUTS],
) -> anyhow::Result<Circuit4ProofOutput> {
    generate_circuit4_proof_with_transcript(
        key_manager,
        circuit,
        instances,
        TranscriptKind::Blake2b,
    )
}

/// Generate a Circuit 4 proof with the chosen transcript.
pub fn generate_circuit4_proof_with_transcript(
    key_manager: &mut KeyManager,
    circuit: BridgeEventProveCircuit,
    instances: [Fr; CIRCUIT4_NUM_PUBLIC_INPUTS],
    transcript: TranscriptKind,
) -> anyhow::Result<Circuit4ProofOutput> {
    key_manager.ensure_event_keys()?;
    key_manager.load_event_pk()?;

    info!(?transcript, "generating circuit-4 withdrawal proof");

    let instance_refs: &[&[Fr]] = &[&instances];
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
                key_manager.event_pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("circuit-4 proof failed (Blake2b transcript)")?;
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
                key_manager.event_pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("circuit-4 proof failed (Poseidon transcript)")?;
            t.finalize()
        },
    };

    Ok(Circuit4ProofOutput {
        proof_bytes,
        instances,
    })
}
