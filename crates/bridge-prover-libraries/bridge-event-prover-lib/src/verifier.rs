//! Circuit 4 (Event Prove — `WithdrawalInitiated`) proof verification, plus
//! `BridgeMultiHopProof` + whole-bundle verification for cross-thread claims.
//!
//! The halo2/KZG verification stack is identical to Circuits 1a and 2, so
//! we delegate to [`bridge_prover_lib::verifier::verify_kzg_proof`] and
//! supply Circuit 4's VK from the [`EventKeyManager`] (or the multi-hop
//! VK from [`MultiHopKeyManager`]).

use bridge_event_prove_circuit::bundle_verifier::{
    verify_bundle as circuit_verify_bundle, BundleError, BundleProof,
};
use bridge_prover_lib::{
    keys::{EventKeyManager, MultiHopKeyManager},
    transcript::TranscriptKind,
};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use thiserror::Error;

use crate::bundle::EventBundle;

/// Verify a Circuit 4 proof against its public instances (Blake2b transcript).
///
/// Instances layout (length `TOTAL_PUBLIC_INPUTS = 13`):
///   `[token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
///   sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root, anchor_layer,
///   x_block_id, y_block_id]`
///
/// The instance count is checked implicitly by `verify_proof` against the
/// VK shape.
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

/// Verify a `BridgeMultiHopProof` against its 2-slot public instances
/// (`[hopStartBlockId, hopEndBlockId]`) using the Blake2b transcript.
pub fn verify_multi_hop_proof(
    mh_km: &MultiHopKeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof(mh_km.srs(), mh_km.vk(), proof_bytes, instances)
}

/// Verify a `BridgeMultiHopProof` with the chosen Fiat–Shamir transcript.
pub fn verify_multi_hop_proof_with_transcript(
    mh_km: &MultiHopKeyManager,
    proof_bytes: &[u8],
    instances: &[Fr],
    transcript: TranscriptKind,
) -> bool {
    bridge_prover_lib::verifier::verify_kzg_proof_with_transcript(
        mh_km.srs(),
        mh_km.vk(),
        proof_bytes,
        instances,
        transcript,
    )
}

/// Reasons `verify_bundle` may reject a bundle. Splits into a *structural*
/// class delegated to the circuit-side spec verifier and per-snark SHPLONK
/// failures raised by this crate.
#[derive(Debug, Error)]
pub enum BundleVerifyError {
    /// The spec-executable off-circuit gate rejected the bundle. See
    /// `bridge_event_prove_circuit::bundle_verifier::BundleError` for the
    /// full taxonomy.
    #[error("bundle structural check failed: {0:?}")]
    Structural(BundleError),
    /// The final `BridgeEventFinalProof` snark failed KZG verification.
    #[error("final snark KZG verify failed")]
    FinalSnarkInvalid,
    /// A `BridgeMultiHopProof` snark at bundle index `index` (1-based over
    /// hop slots, so `index = 0` is the first hop snark) failed KZG
    /// verification.
    #[error("multi-hop snark #{index} KZG verify failed")]
    HopSnarkInvalid { index: usize },
}

/// End-to-end verification of an `EventBundle`:
///
/// 1. Convert the bundle to the circuit-side `BundleProof` layout, then
///    delegate to `bridge_event_prove_circuit::bundle_verifier::verify_bundle`
///    for the *structural* checks (final-first, adjacency, endpoint glue,
///    anchor gate).
/// 2. Verify each snark's SHPLONK proof against the matching VK (final with
///    `event_km.vk()`, each hop with `mh_km.vk()`).
///
/// `anchor_ok` receives `(final_root_bytes, anchor_layer_u8)` and returns
/// true iff the pair is a known layer anchor — off-chain callers wire this
/// through `bridge-verifier-daemon`'s `layer_windows` snapshot, on-chain
/// it is `AckiNackiBridge._isKnownLayerAnchor`.
pub fn verify_bundle(
    event_km: &EventKeyManager,
    mh_km: &MultiHopKeyManager,
    bundle: &EventBundle,
    anchor_ok: impl Fn(&[u8; 32], u8) -> bool,
) -> Result<(), BundleVerifyError> {
    let mut proofs: Vec<BundleProof> = Vec::with_capacity(1 + bundle.hop_blobs.len());
    proofs.push(BundleProof::new_final(
        bundle.final_public_instances.clone(),
    ));
    for hop_pi in &bundle.hop_public_instances {
        proofs.push(BundleProof::new_multi_hop(hop_pi.clone()));
    }

    circuit_verify_bundle(&proofs, anchor_ok).map_err(BundleVerifyError::Structural)?;

    if !verify_event_proof(event_km, &bundle.final_blob, &bundle.final_public_instances) {
        return Err(BundleVerifyError::FinalSnarkInvalid);
    }

    for (i, (hop_blob, hop_pi)) in bundle
        .hop_blobs
        .iter()
        .zip(bundle.hop_public_instances.iter())
        .enumerate()
    {
        if !verify_multi_hop_proof(mh_km, hop_blob, hop_pi) {
            return Err(BundleVerifyError::HopSnarkInvalid {
                index: i,
            });
        }
    }

    Ok(())
}
