//! Circuit 1B (Fallback attestation) proof generation.
//!
//! Mirrors `bridge_prover_lib::prover::generate_primary_proof` but takes two attestations
//! (Primary-typed + Fallback-typed) and uses `FallbackAttestationBlsCheckerCircuit`.

use std::collections::HashMap;

use anyhow::Context;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::info;

use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};

use crate::keys::FallbackKeyManager;
use crate::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers,
    circuit_num_limbs, circuit_num_unusable_rows,
};

/// Output of a fallback proof generation.
#[derive(Debug, Clone)]
pub struct FallbackProofOutput {
    pub proof_bytes: Vec<u8>,
    pub envelope_hash_fr: Fr,
    pub bk_set_commitment_fr: Fr,
    pub block_seq_no: u32,
    pub last_seen_block_seqno: u32,
}

impl FallbackProofOutput {
    /// Public-instance vector in the order Circuit 1B emits:
    /// `[envelope_hash, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
    pub fn instances(&self) -> [Fr; 4] {
        [
            self.envelope_hash_fr,
            self.bk_set_commitment_fr,
            Fr::from(self.block_seq_no as u64),
            Fr::from(self.last_seen_block_seqno as u64),
        ]
    }
}

/// Generate a Circuit 1B (fallback attestation) proof.
///
/// # Arguments
/// - `key_manager` — VK/PK/SRS for Circuit 1B (must have run `ensure_keys` first).
/// - `attestation_primary_bytes` — serialized Envelope<AttestationData> with target_type = Primary.
/// - `attestation_fallback_bytes` — serialized Envelope<AttestationData> with target_type = Fallback.
///   Both attestations MUST reference the same envelope_hash (the circuit constrains this).
/// - `bk_set` — current BK set: signer_index → 48-byte compressed BLS pubkey.
/// - `last_seen_block_seqno` — must be < block_seq_no extracted from the primary attestation.
pub fn generate_fallback_proof(
    key_manager: &FallbackKeyManager,
    attestation_primary_bytes: &[u8],
    attestation_fallback_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> anyhow::Result<FallbackProofOutput> {
    let envelope_hash_fr = compute_envelope_hash_fr(attestation_primary_bytes);
    let bk_set_commitment_fr =
        crate::compute_bk_set_poseidon(bk_set, circuit_limb_bits(), circuit_num_limbs());
    let block_seq_no = extract_block_seq_no(attestation_primary_bytes);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);

    info!(
        block_seq_no,
        last_seen_block_seqno,
        bk_set_size = bk_set.len(),
        "generating fallback proof"
    );

    let mut circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
        attestation_primary_bytes.to_vec(),
        attestation_fallback_bytes.to_vec(),
        bk_set.clone(),
        last_seen_block_seqno,
        circuit_k() as usize,
        circuit_num_unusable_rows(),
        circuit_lookup_bits(),
        circuit_limb_bits(),
        circuit_num_limbs(),
        circuit_max_signers(),
    );
    circuit.override_base_circuit_params(key_manager.config().clone());

    let instances = vec![envelope_hash_fr, bk_set_commitment_fr, block_seq_no_fr, last_seen_fr];
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
        key_manager.pk(),
        &[circuit],
        &[instance_refs],
        OsRng,
        &mut transcript,
    )
    .context("fallback proof generation failed")?;
    let proof_bytes = transcript.finalize();

    Ok(FallbackProofOutput {
        proof_bytes,
        envelope_hash_fr,
        bk_set_commitment_fr,
        block_seq_no,
        last_seen_block_seqno,
    })
}

/// Extract envelope_hash as Fr from raw attestation bytes. (Identical to partner's primary
/// helper — same offset, same little-endian reduction.)
fn compute_envelope_hash_fr(attestation_bytes: &[u8]) -> Fr {
    const ENVELOPE_HASH_REL_OFFSET: usize = 84;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + ENVELOPE_HASH_REL_OFFSET;
    let env_hash_bytes = &attestation_bytes[abs_offset..abs_offset + 32];

    let mut result = Fr::zero();
    let mut power = Fr::one();
    let base = Fr::from(256u64);
    for &byte in env_hash_bytes {
        result += Fr::from(byte as u64) * power;
        power *= base;
    }
    result
}

/// Extract block_seq_no (u32 LE) from raw attestation bytes.
fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
