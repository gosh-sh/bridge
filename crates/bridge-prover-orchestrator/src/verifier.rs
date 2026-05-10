//! Circuit 1B native Halo2 SHPLONK verification (off-chain).
//!
//! Same transcript and strategy as `bridge_prover_lib::verifier::verify_primary_proof`,
//! just keyed by [`FallbackKeyManager`].

use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::verify_proof,
    poly::{
        commitment::ParamsProver,
        kzg::{
            commitment::KZGCommitmentScheme, multiopen::VerifierSHPLONK, strategy::SingleStrategy,
        },
    },
    transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer},
};

use crate::keys::FallbackKeyManager;

/// Verify a Circuit 1B (fallback) proof against `instances`.
///
/// `instances` must be `[block_id, bk_set_poseidon, block_seq_no, last_seen_block_seqno]`
/// in that order — same layout the prover emits via `FallbackProofOutput::instances`.
pub fn verify_fallback_proof(
    key_manager: &FallbackKeyManager,
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
