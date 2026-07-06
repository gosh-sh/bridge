//! Circuit 4 (Event Prove — `WithdrawalInitiated`) proof verification.
//!
//! The halo2/KZG verification stack is identical to Circuits 1a and 2, so
//! we delegate to [`bridge_prover_lib::verifier::verify_kzg_proof`] and
//! supply Circuit 4's VK from the shared [`KeyManager`].

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::transcript::TranscriptKind;

/// Verify a Circuit 4 proof against its public instances (Blake2b transcript).
///
/// Instances layout (length `TOTAL_PUBLIC_INPUTS = 10`):
///   `[token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
///   sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root]`
///
/// The instance count is checked implicitly by `verify_proof` against the
/// VK shape.
pub fn verify_event_proof(
    key_manager: &KeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof(
        key_manager,
        key_manager.event_vk(),
        proof_bytes,
        instances,
    )
}

/// Verify a Circuit 4 proof with the chosen Fiat–Shamir transcript. Must
/// match what
/// [`crate::prover::generate_event_proof_with_transcript`] used or
/// verification returns `false`.
pub fn verify_event_proof_with_transcript(
    key_manager: &KeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
    transcript: TranscriptKind,
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof_with_transcript(
        key_manager,
        key_manager.event_vk(),
        proof_bytes,
        instances,
        transcript,
    )
}
