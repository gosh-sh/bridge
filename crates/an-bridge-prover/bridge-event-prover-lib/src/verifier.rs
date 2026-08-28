//! Circuit 4 (Event Prove — `WithdrawalInitiated`) proof verification.
//!
//! The halo2/KZG verification stack is identical to Circuits 1a and 2, so
//! we delegate to [`bridge_prover_lib::verifier::verify_kzg_proof`] and
//! supply Circuit 4's VK from the [`EventKeyManager`].

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

use bridge_prover_lib::keys::EventKeyManager;
use bridge_prover_lib::transcript::TranscriptKind;

/// Verify a Circuit 4 proof against its public instances (Blake2b transcript).
///
/// Instances layout (length `TOTAL_PUBLIC_INPUTS = 10`):
///   `[token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
///   sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root]`
///
/// The instance count is checked implicitly by `verify_proof` against the
/// VK shape.
/// 
pub fn verify_event_proof(
    event_km: &EventKeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof(
        event_km.srs(),
        event_km.vk(),
        proof_bytes,
        instances,
    )
}

/// Verify a Circuit 4 proof with the chosen Fiat–Shamir transcript. 
pub fn verify_event_proof_with_transcript(
    event_km: &EventKeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
    transcript: TranscriptKind,
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof_with_transcript(
        event_km.srs(),
        event_km.vk(),
        proof_bytes,
        instances,
        transcript,
    )
}
