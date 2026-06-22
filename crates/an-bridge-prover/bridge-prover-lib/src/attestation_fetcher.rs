//! Fetch attestations for a target block via GraphQL `Block.attestations[]`.
//!
//! The v3 gql-server exposes `Block.attestations[]` as a consumer-oriented view: the
//! resolver filters entries so each block's array contains attestations whose
//! inner `AttestationData.block_id` matches that block's own id — i.e. "the
//! attestation that signed THIS block". Behind the scenes the row still lives
//! in a later block's `common_section.block_attestations()`, but the GQL view
//! hides that and we just query block N directly.

use std::collections::HashMap;

use anyhow::Context;
use tracing::info;

use crate::gql_client::GqlClient;

/// An attestation envelope assembled from GraphQL fields on a later block's
/// `attestations[]` array.
///
/// `raw_bytes` is laid out exactly as `bincode(Envelope<AttestationData>)` so
/// `bridge_parsers::attestation_data_parser` and `prover.rs::compute_block_id_fr`
/// can index it with their fixed offsets.
#[derive(Debug, Clone)]
pub struct ParsedAttestation {
    pub raw_bytes: Vec<u8>,
    pub parent_block_id: [u8; 32],
    pub block_id: [u8; 32],
    pub block_seq_no: u32,
    pub envelope_hash: [u8; 32],
    pub target_type: u32, // 0 = Primary, 1 = Fallback
    pub signature_occurrences: HashMap<u16, u16>,
}

/// Fetch the attestation envelope for block `target_seq_no` from GraphQL.
///
/// Legacy entry point that returns a single attestation. Picks the PRIMARY
/// entry if present, else the first parseable one. For full path-aware
/// classification (Primary vs Fallback) use [`fetch_attestation_evidence`].
pub async fn fetch_attestation_for_block(
    client: &GqlClient,
    target_seq_no: u32,
) -> anyhow::Result<ParsedAttestation> {
    let mut att = client
        .query_attestation_envelope(target_seq_no as u64)
        .await
        .with_context(|| format!("query_attestation_envelope({target_seq_no})"))?;

    // The GraphQL `BlockAttestation` row doesn't expose block_seq_no; patch it
    // in here so downstream consumers (prover.rs::extract_block_seq_no) read
    // the correct value from raw_bytes.
    patch_seq_no_in_raw_bytes(&mut att, target_seq_no);
    att.block_seq_no = target_seq_no;

    info!(
        "attestation for seq={}: type={}, signers={:?}",
        target_seq_no,
        if att.target_type == 0 { "Primary" } else { "Fallback" },
        att.signature_occurrences,
    );

    Ok(att)
}

/// Classified attestation evidence for a key block.
///
/// Acki Nacki finalization is a two-path protocol (consensus-protocol.md §4.6):
///   * **Primary path** — reached ≥2N/3 signers within the `β`-block deadline.
///     `Block.attestations[]` contains exactly one entry of `target_type=PRIMARY`.
///   * **Fallback path** — primary deadline passed without ≥2N/3; the chain
///     then collects a `PRIMARY`-type prefinalization proof at `β` plus a
///     `FALLBACK`-type target proof at `2β`, each ≥N/2+1 signers, both over
///     the same `block_id`. The block is only fallback-finalized when both
///     attestations exist.
///
/// Mapping to bridge circuits:
///   * `Primary`  → Circuit 1A (`PrimaryAttestationBlsCheckerCircuit`,  threshold ≥2N/3)
///   * `Fallback` → Circuit 1B (`FallbackAttestationBlsCheckerCircuit`, threshold > N/2)
///
/// Both circuits produce a proof over the same 4 public instances
/// `[block_id, bk_set_poseidon, block_seq_no, last_seen]` — only the
/// verifying key differs.
#[derive(Debug, Clone)]
pub enum AttestationEvidence {
    /// One PRIMARY-type attestation. Threshold enforcement (≥2N/3) lives
    /// inside Circuit 1A; the daemon does not pre-check signer counts.
    Primary(ParsedAttestation),
    /// Pair of attestations (PRIMARY prefinalization + FALLBACK target) over
    /// the same `block_id`. Threshold (>N/2 each) is enforced inside
    /// Circuit 1B via `ThresholdMode::Fallback`.
    Fallback {
        primary: ParsedAttestation,
        fallback: ParsedAttestation,
    },
}

impl AttestationEvidence {
    /// Human-readable tag for logs / IPC.
    pub fn path(&self) -> &'static str {
        match self {
            AttestationEvidence::Primary(_) => "primary",
            AttestationEvidence::Fallback { .. } => "fallback",
        }
    }

    /// `block_id` shared by every attestation in the evidence. For the
    /// fallback variant this is asserted equal across the two entries during
    /// construction.
    pub fn block_id(&self) -> [u8; 32] {
        match self {
            AttestationEvidence::Primary(p) => p.block_id,
            AttestationEvidence::Fallback { primary, .. } => primary.block_id,
        }
    }

    /// Union of signer indices across all attestations in the evidence. The
    /// daemon uses this for BK-set-membership sanity warnings before
    /// dispatching to the prover (in-circuit checks handle correctness; this
    /// is only an early-warning path).
    pub fn signer_indices(&self) -> std::collections::HashSet<u16> {
        match self {
            AttestationEvidence::Primary(p) => p.signature_occurrences.keys().copied().collect(),
            AttestationEvidence::Fallback { primary, fallback } => {
                let mut s: std::collections::HashSet<u16> =
                    primary.signature_occurrences.keys().copied().collect();
                s.extend(fallback.signature_occurrences.keys().copied());
                s
            }
        }
    }
}

/// Fetch and classify the attestation evidence for `target_seq_no`.
///
/// Detection is **structural**, not heuristic: the chain only ever emits
///   * `[PRIMARY]`                              → `Primary` (Circuit 1A)
///   * `[PRIMARY, FALLBACK]` same `block_id`    → `Fallback` (Circuit 1B)
///
/// Anything else (zero attestations, two PRIMARY entries, mismatched
/// block_ids in the pair, etc.) is treated as a transient or malformed
/// state and returned as an error so the daemon retries instead of
/// silently choosing the wrong circuit.
pub async fn fetch_attestation_evidence(
    client: &GqlClient,
    target_seq_no: u32,
) -> anyhow::Result<AttestationEvidence> {
    let mut atts = client
        .query_attestation_envelopes(target_seq_no as u64)
        .await
        .with_context(|| format!("query_attestation_envelopes({target_seq_no})"))?;

    for a in atts.iter_mut() {
        patch_seq_no_in_raw_bytes(a, target_seq_no);
        a.block_seq_no = target_seq_no;
    }

    let (primaries, fallbacks): (Vec<_>, Vec<_>) =
        atts.into_iter().partition(|a| a.target_type == 0);

    match (primaries.len(), fallbacks.len()) {
        (1, 0) => {
            let p = primaries.into_iter().next().unwrap();
            info!(
                "block {}: PRIMARY path ({} unique signers)",
                target_seq_no,
                p.signature_occurrences.len()
            );
            Ok(AttestationEvidence::Primary(p))
        }
        (1, 1) => {
            let primary = primaries.into_iter().next().unwrap();
            let fallback = fallbacks.into_iter().next().unwrap();
            anyhow::ensure!(
                primary.block_id == fallback.block_id,
                "block {target_seq_no}: fallback pair block_id mismatch \
                 (primary={:?}, fallback={:?})",
                hex::encode(primary.block_id),
                hex::encode(fallback.block_id),
            );
            info!(
                "block {}: FALLBACK path ({} primary signers, {} fallback signers)",
                target_seq_no,
                primary.signature_occurrences.len(),
                fallback.signature_occurrences.len(),
            );
            Ok(AttestationEvidence::Fallback { primary, fallback })
        }
        (np, nf) => anyhow::bail!(
            "block {target_seq_no}: unexpected attestations shape \
             (primaries={np}, fallbacks={nf}); expected [PRIMARY] or [PRIMARY, FALLBACK]"
        ),
    }
}

/// Overwrite the 4-byte block_seq_no slot inside `att.raw_bytes`. Layout:
/// `[0..200] = aggregated_signature`; `[200..208+N*4] = signature_occurrences`;
/// the attestation-data section begins at `208 + num_signers*4` and the
/// `block_seq_no` u32 sits at relative offset `80` within that section.
fn patch_seq_no_in_raw_bytes(att: &mut ParsedAttestation, seq_no: u32) {
    let num_signers: usize = att.signature_occurrences.values().map(|c| *c as usize).sum();
    let data_offset = 208 + num_signers * 4;
    let seq_off = data_offset + 80;
    if seq_off + 4 <= att.raw_bytes.len() {
        att.raw_bytes[seq_off..seq_off + 4].copy_from_slice(&seq_no.to_le_bytes());
    }
}
