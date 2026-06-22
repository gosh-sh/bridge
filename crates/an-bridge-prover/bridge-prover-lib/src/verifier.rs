//! Halo2/KZG proof verification for the bridge prover circuits.
//!
//! Both circuits served by this crate (Circuit 1a "Primary Attestation" and
//! Circuit 2 "Layer Historical Hashes Movement Checker") share the exact same
//! verifier stack — Blake2b transcript, SHPLONK multiopen, single-strategy,
//! Bn256/KZG. They differ only in which `VerifyingKey` from the `KeyManager`
//! is selected and in the expected public-instance length (enforced
//! implicitly by halo2 against the VK shape, so we don't re-check it here).
//!
//! The two `verify_*` entry points are thin wrappers around the shared
//! [`verify_kzg_proof`] helper so the verification stack stays defined in
//! exactly one place. The helper is `pub` because Circuit 4 (Event Prove)
//! in `bridge-event-prover-lib` reuses it verbatim — same stack, different VK.

use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{verify_proof, VerifyingKey},
    poly::{
        commitment::ParamsProver,
        kzg::{
            commitment::{KZGCommitmentScheme, ParamsKZG},
            multiopen::VerifierSHPLONK,
            strategy::SingleStrategy,
        },
    },
    transcript::{Blake2bRead, Challenge255, EncodedChallenge, TranscriptRead, TranscriptReadBuffer},
};

use crate::keys::KeyManager;

/// Drive Halo2 KZG/SHPLONK verification with a caller-supplied Fiat–Shamir
/// transcript reader. Mirror of [`crate::prover::create_proof_with_transcript`]
/// for the verify path — same motivation: lets downstream crates supply a
/// non-Blake2b transcript (e.g. Poseidon for snark-verifier consumption) while
/// still routing through this crate's shared verification stack
/// (Bn256/KZG/SHPLONK + `SingleStrategy`).
pub fn verify_proof_with_transcript<E, T>(
    srs: &ParamsKZG<Bn256>,
    vk: &VerifyingKey<G1Affine>,
    instances: &[Fr],
    transcript: &mut T,
) -> bool
where
    E: EncodedChallenge<G1Affine>,
    T: TranscriptRead<G1Affine, E>,
{
    let instance_refs: &[&[Fr]] = &[instances];
    let verifier_params = srs.verifier_params();
    let strategy = SingleStrategy::new(srs);
    verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        E,
        T,
        SingleStrategy<'_, Bn256>,
    >(
        verifier_params,
        vk,
        strategy,
        &[instance_refs],
        transcript,
    )
    .is_ok()
}

/// Shared verification core. All circuit-specific wrappers delegate here
/// so the transcript / strategy / multiopen choices live in exactly one
/// place. Re-exported `pub` for the Circuit 4 verifier in
/// `bridge-event-prover-lib`.
pub fn verify_kzg_proof(
    key_manager: &KeyManager,
    vk: &VerifyingKey<G1Affine>,
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
        vk,
        strategy,
        &[instance_refs],
        &mut transcript,
    )
    .is_ok()
}

/// Verify a Circuit 1a (Primary Attestation) proof against the given
/// 4 public instances: `[block_id, bk_set_commitment, block_seq_no, last_seen]`.
pub fn verify_primary_proof(
    key_manager: &KeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    verify_kzg_proof(key_manager, key_manager.primary_vk(), proof_bytes, instances)
}

/// Verify a Circuit 1b (Fallback Attestation) proof. Same 4 public instances
/// as Circuit 1a — only the verifying key differs, since the fallback
/// circuit's constraint system covers two BLS verifications (PRIMARY
/// prefinalization + FALLBACK target proof) plus same-block_id equality
/// checks, all bound to the fallback VK at keygen.
pub fn verify_fallback_proof(
    key_manager: &KeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    verify_kzg_proof(key_manager, key_manager.fallback_vk(), proof_bytes, instances)
}

/// Verify a Circuit 2 (Layer Historical Hashes Movement Checker) proof
/// against the given 14 public instances:
/// `[block_id, bk_set_poseidon_hash, num_layers, layer_hash_frs[0..9], prev_max_level_layer_hash]`.
pub fn verify_layer_proof(
    key_manager: &KeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    verify_kzg_proof(key_manager, key_manager.layer_vk(), proof_bytes, instances)
}
