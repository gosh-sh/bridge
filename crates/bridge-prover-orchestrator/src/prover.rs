//! Circuit 1B (Fallback attestation) proof generation.
//!
//! Mirrors `bridge_prover_lib::prover::generate_primary_proof` but takes two
//! attestations (Primary-typed + Fallback-typed) and uses
//! `FallbackAttestationBlsCheckerCircuit`.

use std::collections::HashMap;

use anyhow::Context;
use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use bridge_parsers::attestation_data_parser::{attestation_data_offset, parse_num_signers};
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::info;

use crate::{
    circuit_k, circuit_limb_bits, circuit_lookup_bits, circuit_max_signers, circuit_num_limbs,
    circuit_num_unusable_rows, halo2_tvm_bundle::TranscriptKind, keys::FallbackKeyManager,
    poseidon_transcript::PoseidonWrite,
};

/// Output of a fallback proof generation.
///
/// Public-instance [0] is `block_id` since 2026-05-10 (was `envelope_hash`
/// before the partner's rename in
/// `acki-nacki-to-eth-bridge-halo2-circuits` commit `672854b`). The 32-byte
/// field is now read at relative offset 48 inside `AttestationData`, not 84.
#[derive(Debug, Clone)]
pub struct FallbackProofOutput {
    pub proof_bytes: Vec<u8>,
    pub block_id_fr: Fr,
    pub bk_set_commitment_fr: Fr,
    pub block_seq_no: u32,
    pub last_seen_block_seqno: u32,
}

impl FallbackProofOutput {
    /// Public-instance vector in the order Circuit 1B emits:
    /// `[block_id, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`.
    pub fn instances(&self) -> [Fr; 4] {
        [
            self.block_id_fr,
            self.bk_set_commitment_fr,
            Fr::from(self.block_seq_no as u64),
            Fr::from(self.last_seen_block_seqno as u64),
        ]
    }
}

/// Generate a Circuit 1B (fallback attestation) proof.
///
/// # Arguments
/// - `key_manager` — VK/PK/SRS for Circuit 1B (must have run `ensure_keys`
///   first).
/// - `attestation_primary_bytes` — serialized Envelope<AttestationData> with
///   target_type = Primary.
/// - `attestation_fallback_bytes` — serialized Envelope<AttestationData> with
///   target_type = Fallback. Both attestations MUST reference the same
///   `block_id` (the circuit constrains this byte-by-byte).
/// - `bk_set` — current BK set: signer_index → 48-byte compressed BLS pubkey.
/// - `last_seen_block_seqno` — must be < block_seq_no extracted from the
///   primary attestation.
pub fn generate_fallback_proof(
    key_manager: &FallbackKeyManager,
    attestation_primary_bytes: &[u8],
    attestation_fallback_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> anyhow::Result<FallbackProofOutput> {
    generate_fallback_proof_with_transcript(
        key_manager,
        attestation_primary_bytes,
        attestation_fallback_bytes,
        bk_set,
        last_seen_block_seqno,
        TranscriptKind::Blake2b,
    )
}

/// Generate a Circuit 1B proof with the chosen Fiat–Shamir transcript.
///
/// - [`TranscriptKind::Blake2b`] — default, matches what `ZKHALO2VERIFYWITHVK`
///   expects on the AN side.
/// - [`TranscriptKind::Poseidon`] — for ETH-side aggregator consumption (R15
///   pipeline, `crates/bridge-evm-aggregator/`). Proofs in this flavour MUST
///   NOT be shipped to the AN side; the opcode rejects them.
pub fn generate_fallback_proof_with_transcript(
    key_manager: &FallbackKeyManager,
    attestation_primary_bytes: &[u8],
    attestation_fallback_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
    transcript: TranscriptKind,
) -> anyhow::Result<FallbackProofOutput> {
    let block_id_fr = compute_block_id_fr(attestation_primary_bytes);
    let (bk_set_commitment_fr, _) = crate::compute_bk_set_poseidon(bk_set);
    let block_seq_no = extract_block_seq_no(attestation_primary_bytes);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);

    info!(
        block_seq_no,
        last_seen_block_seqno,
        bk_set_size = bk_set.len(),
        ?transcript,
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
                key_manager.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("fallback proof generation failed (Blake2b transcript)")?;
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
            .context("fallback proof generation failed (Poseidon transcript)")?;
            t.finalize()
        },
    };

    Ok(FallbackProofOutput {
        proof_bytes,
        block_id_fr,
        bk_set_commitment_fr,
        block_seq_no,
        last_seen_block_seqno,
    })
}

/// Extract `block_id` as Fr from raw attestation bytes (offset 48..80, since
/// the partner's 2026-05-10 rename of `env_hash_cells → block_id_cells`).
/// Mirrors `bridge_prover_lib::prover::compute_block_id_fr` byte-for-byte.
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

/// Extract block_seq_no (u32 LE) from raw attestation bytes.
fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
