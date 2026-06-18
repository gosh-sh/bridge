use std::collections::HashMap;

use anyhow::Context;
use halo2_base::halo2_proofs::{
    dev::MockProver,
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{create_proof, Circuit, ProvingKey},
    poly::kzg::{commitment::{KZGCommitmentScheme, ParamsKZG}, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, EncodedChallenge, TranscriptWrite, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::{info, warn};

use attestation_bls_checker_circuit::primary_circuit::PrimaryAttestationBlsCheckerCircuit;
use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use bridge_parsers::attestation_data_parser::{
    attestation_data_offset, parse_num_signers,
};

use crate::keys::{self, KeyManager};
use crate::poseidon::compute_bk_set_poseidon;

/// Output of a proof generation.
#[derive(Debug, Clone)]
pub struct ProofOutput {
    pub proof_bytes: Vec<u8>,
    pub block_id_fr: Fr,
    pub bk_set_commitment_fr: Fr,
    pub block_seq_no: u32,
    pub last_seen_block_seqno: u32,
}

/// Generate a primary attestation proof.
pub fn generate_primary_proof(
    key_manager: &KeyManager,
    attestation_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> anyhow::Result<ProofOutput> {
    let limb_bits = keys::circuit_limb_bits();
    let num_limbs = keys::circuit_num_limbs();

    // Compute expected public instances.
    let block_id_fr = compute_block_id_fr(attestation_bytes);
    let (bk_set_commitment_fr, _) = compute_bk_set_poseidon(bk_set);
    let block_seq_no = extract_block_seq_no(attestation_bytes);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);

    info!(
        "generating proof: block_seq_no={}, last_seen={}, bk_set_size={}",
        block_seq_no, last_seen_block_seqno, bk_set.len()
    );

    // Build circuit.
    let mut circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
        attestation_bytes.to_vec(),
        bk_set.clone(),
        last_seen_block_seqno,
        keys::circuit_k() as usize,
        keys::circuit_num_unusable_rows(),
        keys::circuit_lookup_bits(),
        limb_bits,
        num_limbs,
        keys::circuit_max_signers(),
    );
    circuit.override_base_circuit_params(key_manager.primary_config().clone());

    // Generate proof.
    let instances = vec![block_id_fr, bk_set_commitment_fr, block_seq_no_fr, last_seen_fr];

    // Optional MockProver diagnostic. Gated by env to keep normal runs fast (k=20 is
    // very slow under MockProver). Set BRIDGE_MOCK_PROVE=1 to enable.
    if std::env::var("BRIDGE_MOCK_PROVE").ok().as_deref() == Some("1") {
        let k = keys::circuit_k();
        info!("BRIDGE_MOCK_PROVE=1: running MockProver at k={} (this can take minutes)...", k);
        let t = std::time::Instant::now();
        match MockProver::run(k, &circuit, vec![instances.clone()]) {
            Ok(prover) => match prover.verify() {
                Ok(()) => info!("MockProver verify OK ({:?})", t.elapsed()),
                Err(failures) => {
                    warn!(
                        "MockProver verify FAILED ({:?}): {} failure(s)",
                        t.elapsed(),
                        failures.len()
                    );
                    for (i, f) in failures.iter().take(10).enumerate() {
                        warn!("  failure[{}]: {:?}", i, f);
                    }
                    if failures.len() > 10 {
                        warn!("  ... ({} more failures suppressed)", failures.len() - 10);
                    }
                }
            },
            Err(e) => warn!("MockProver::run errored: {:?}", e),
        }
    }

    let proof_bytes = run_kzg_create_proof(
        key_manager,
        key_manager.primary_pk(),
        circuit,
        &instances,
    )?;

    Ok(ProofOutput {
        proof_bytes,
        block_id_fr,
        bk_set_commitment_fr,
        block_seq_no,
        last_seen_block_seqno,
    })
}

/// Generate a fallback attestation proof (Circuit 1b).
///
/// Consumes both attestations from the fallback evidence pair:
///   * `attestation_primary_bytes`  — PRIMARY-type prefinalization (>N/2 signers)
///   * `attestation_fallback_bytes` — FALLBACK-type target proof  (>N/2 signers)
///
/// Both share the same `block_id`; that equality is enforced in-circuit by
/// `FallbackAttestationBlsCheckerCircuit`. The public-instance shape is
/// identical to Circuit 1a (`[block_id, bk_set_commitment, block_seq_no,
/// last_seen]`), so downstream verifiers and IPC consumers only need to
/// discriminate via the verifying key (1a vs 1b) — not via instance layout.
pub fn generate_fallback_proof(
    key_manager: &KeyManager,
    attestation_primary_bytes: &[u8],
    attestation_fallback_bytes: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    last_seen_block_seqno: u32,
) -> anyhow::Result<ProofOutput> {
    let limb_bits = keys::circuit_limb_bits();
    let num_limbs = keys::circuit_num_limbs();

    // Public instances are derived from the PRIMARY half of the pair; the
    // FALLBACK half is constrained in-circuit to share the same block_id.
    let block_id_fr = compute_block_id_fr(attestation_primary_bytes);
    let (bk_set_commitment_fr, _) = compute_bk_set_poseidon(bk_set);
    let block_seq_no = extract_block_seq_no(attestation_primary_bytes);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);

    info!(
        "generating fallback proof: block_seq_no={}, last_seen={}, bk_set_size={}, \
         primary_sig_len={}, fallback_sig_len={}",
        block_seq_no, last_seen_block_seqno, bk_set.len(),
        attestation_primary_bytes.len(), attestation_fallback_bytes.len(),
    );

    let mut circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
        attestation_primary_bytes.to_vec(),
        attestation_fallback_bytes.to_vec(),
        bk_set.clone(),
        last_seen_block_seqno,
        keys::circuit_k() as usize,
        keys::circuit_num_unusable_rows(),
        keys::circuit_lookup_bits(),
        limb_bits,
        num_limbs,
        keys::circuit_max_signers(),
    );
    circuit.override_base_circuit_params(key_manager.fallback_config().clone());

    let instances = vec![block_id_fr, bk_set_commitment_fr, block_seq_no_fr, last_seen_fr];

    if std::env::var("BRIDGE_MOCK_PROVE").ok().as_deref() == Some("1") {
        let k = keys::circuit_k();
        info!("BRIDGE_MOCK_PROVE=1: running MockProver (fallback) at k={}...", k);
        let t = std::time::Instant::now();
        match MockProver::run(k, &circuit, vec![instances.clone()]) {
            Ok(prover) => match prover.verify() {
                Ok(()) => info!("MockProver (fallback) verify OK ({:?})", t.elapsed()),
                Err(failures) => {
                    warn!("MockProver (fallback) verify FAILED ({:?}): {} failure(s)",
                          t.elapsed(), failures.len());
                    for (i, f) in failures.iter().take(10).enumerate() {
                        warn!("  failure[{}]: {:?}", i, f);
                    }
                }
            },
            Err(e) => warn!("MockProver::run errored: {:?}", e),
        }
    }

    let proof_bytes = run_kzg_create_proof(
        key_manager,
        key_manager.fallback_pk(),
        circuit,
        &instances,
    )?;

    Ok(ProofOutput {
        proof_bytes,
        block_id_fr,
        bk_set_commitment_fr,
        block_seq_no,
        last_seen_block_seqno,
    })
}

/// Drive Halo2 KZG/SHPLONK proving with a caller-supplied Fiat–Shamir
/// transcript writer.
///
/// The lib's high-level Blake2b paths (`generate_primary_proof`,
/// `generate_fallback_proof`, `generate_layer_proof`) hard-wire the Blake2b
/// transcript via [`run_kzg_create_proof`]. This helper exposes the
/// underlying `create_proof` call so consumers outside this crate (e.g. the
/// gnark-wrapping pipeline in `bridge-prover-orchestrator`, which needs both
/// Blake2b — for the AN-side `ZKHALO2VERIFYWITHVK` opcode — and a Poseidon
/// transcript for snark-verifier consumption) can plug in their own
/// transcript flavour without re-implementing the proving plumbing.
pub fn create_proof_with_transcript<E, T, C>(
    srs: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
    circuit: C,
    instances: &[Fr],
    transcript: &mut T,
) -> anyhow::Result<()>
where
    E: EncodedChallenge<G1Affine>,
    T: TranscriptWrite<G1Affine, E>,
    C: Circuit<Fr>,
{
    let instance_refs: &[&[Fr]] = &[instances];
    create_proof::<
        KZGCommitmentScheme<Bn256>,
        ProverSHPLONK<'_, Bn256>,
        E,
        _,
        T,
        _,
    >(
        srs,
        pk,
        &[circuit],
        &[instance_refs],
        OsRng,
        transcript,
    )
    .context("create_proof_with_transcript: proof generation failed")?;
    Ok(())
}

/// Shared KZG/SHPLONK/Blake2b proof core. The two attestation circuits
/// (1a Primary, 1b Fallback) differ only in their constraint system and
/// proving key; the transcript / multiopen / commitment scheme is identical.
fn run_kzg_create_proof<C>(
    key_manager: &KeyManager,
    pk: &ProvingKey<G1Affine>,
    circuit: C,
    instances: &[Fr],
) -> anyhow::Result<Vec<u8>>
where
    C: Circuit<Fr>,
{
    let instance_refs: &[&[Fr]] = &[instances];
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
        pk,
        &[circuit],
        &[instance_refs],
        OsRng,
        &mut transcript,
    )
    .context("proof generation failed")?;
    Ok(transcript.finalize())
}

/// Extract block_id as Fr from raw attestation bytes.
///
/// block_id is at offset 48 within AttestationData:
/// parent_block_id(40) + length_prefix(8) = 48, then 32 bytes of hash.
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

/// Extract block_seq_no (u32) from raw attestation bytes.
fn extract_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}
