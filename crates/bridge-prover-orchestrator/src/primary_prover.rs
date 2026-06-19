//! Circuit 1A (Primary attestation) proof generation with transcript choice.
//!
//! Mirrors `bridge_prover_lib::prover::generate_primary_proof` but supports
//! Poseidon transcript for the R15 ETH-side aggregator pipeline.

use std::collections::HashMap;

use anyhow::Context;
use attestation_bls_checker_circuit::primary_circuit::PrimaryAttestationBlsCheckerCircuit;
use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::info;

use bridge_prover_lib::keys::KeyManager;

use crate::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers, circuit_num_limbs,
    circuit_num_unusable_rows, halo2_tvm_bundle::TranscriptKind, poseidon_transcript::PoseidonWrite,
};

/// Output of a primary (1A) proof generation.
#[derive(Debug, Clone)]
pub struct PrimaryProofOutput {
    pub proof_bytes: Vec<u8>,
    pub block_id_fr: Fr,
    pub bk_set_commitment_fr: Fr,
    pub block_seq_no: u32,
    pub last_seen_block_seqno: u32,
}

impl PrimaryProofOutput {
    pub fn instances(&self) -> [Fr; 4] {
        [
            self.block_id_fr,
            self.bk_set_commitment_fr,
            Fr::from(self.block_seq_no as u64),
            Fr::from(self.last_seen_block_seqno as u64),
        ]
    }
}

/// Generate Circuit 1A proof (Blake2b transcript — AN-side default).
pub fn generate_primary_proof(
    key_manager: &KeyManager,
    attestation_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> anyhow::Result<PrimaryProofOutput> {
    generate_primary_proof_with_transcript(
        key_manager,
        attestation_bytes,
        bk_set,
        last_seen_block_seqno,
        TranscriptKind::Blake2b,
    )
}

/// Generate Circuit 1A proof with the chosen Fiat–Shamir transcript.
pub fn generate_primary_proof_with_transcript(
    key_manager: &KeyManager,
    attestation_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
    transcript: TranscriptKind,
) -> anyhow::Result<PrimaryProofOutput> {
    let block_id_fr = compute_block_id_fr(attestation_bytes);
    let (bk_set_commitment_fr, _) = crate::compute_bk_set_poseidon(bk_set);
    let block_seq_no = extract_block_seq_no(attestation_bytes);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);

    info!(
        block_seq_no,
        last_seen_block_seqno,
        bk_set_size = bk_set.len(),
        ?transcript,
        "generating primary proof"
    );

    let mut circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
        attestation_bytes.to_vec(),
        bk_set.clone(),
        last_seen_block_seqno,
        circuit_k() as usize,
        circuit_num_unusable_rows(),
        circuit_lookup_bits(),
        circuit_limb_bits(),
        circuit_num_limbs(),
        circuit_max_signers(),
    );
    circuit.override_base_circuit_params(key_manager.primary_config().clone());

    let instances = vec![
        block_id_fr,
        bk_set_commitment_fr,
        block_seq_no_fr,
        last_seen_fr,
    ];
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
                key_manager.primary_pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("primary proof generation failed (Blake2b transcript)")?;
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
                key_manager.primary_pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("primary proof generation failed (Poseidon transcript)")?;
            t.finalize()
        },
    };

    Ok(PrimaryProofOutput {
        proof_bytes,
        block_id_fr,
        bk_set_commitment_fr,
        block_seq_no,
        last_seen_block_seqno,
    })
}

fn compute_block_id_fr(attestation_bytes: &[u8]) -> Fr {
    const BLOCK_ID_REL_OFFSET: usize = 48;
    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_ID_REL_OFFSET;
    let block_id_bytes = &attestation_bytes[abs_offset..abs_offset + 32];
    let mut result = Fr::zero();
    let mut power = Fr::one();
    let base = Fr::from(256u64);
    for &byte in block_id_bytes {
        result += Fr::from(byte as u64) * power;
        power *= base;
    }
    result
}

fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;
    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    u32::from_le_bytes(attestation_bytes[abs_offset..abs_offset + 4].try_into().unwrap())
}
