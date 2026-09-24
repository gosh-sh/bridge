//! Shared test helper for computing the 4 public instances the
//! Primary/Fallback attestation BLS-checker circuits expose.

use std::collections::HashMap;

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

use crate::attestation_data_parser::{compute_block_id_fr, compute_block_seq_no};

/// Compute the 4 public instances expected from a Primary or Fallback circuit,
/// in the order the circuit emits them:
/// `[block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_fr]`.
///
/// `attestation` should be the primary attestation in both cases (fallback
/// circuit re-uses the primary's block_id / block_seq_no for instances).
/// `max_signers` must match the value the circuit constructor was built with —
/// the Poseidon commitment is sensitive to the padding size.
///
/// Returns `(last_seen_block_seqno, instances)` — the seqno is the value the
/// circuit constructor expects as its `last_seen_block_seqno` arg.
pub fn expected_public_instances(
    attestation: &[u8],
    bk_set: &HashMap<u16, Vec<u8>>,
    max_signers: usize,
) -> (u32, Vec<Fr>) {
    let block_id_fr = compute_block_id_fr(attestation);
    let bk_set_commitment =
        bridge_poseidon::compute_bk_set_poseidon_padded_to(bk_set, max_signers).0;
    let block_seq_no = compute_block_seq_no(attestation);
    let last_seen_block_seqno: u32 = block_seq_no.saturating_sub(1);
    let block_seq_no_fr = Fr::from(block_seq_no as u64);
    let last_seen_fr = Fr::from(last_seen_block_seqno as u64);
    (
        last_seen_block_seqno,
        vec![block_id_fr, bk_set_commitment, block_seq_no_fr, last_seen_fr],
    )
}
