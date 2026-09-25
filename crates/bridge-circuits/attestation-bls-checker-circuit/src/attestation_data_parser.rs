// ---------------------------------------------------------------------------
// Off-circuit attestation parsing helpers
//
// Decodes a bincode 1.x-serialized `Envelope<AttestationData>` produced by
// acki-nacki (`bls/envelope.rs` → `EnvelopeSerDe { aggregated_signature,
// signature_occurrences, data }`). See README.md for the full byte layout.
// ---------------------------------------------------------------------------

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

/// Bincode `serialize_bytes` length prefix written before the BLS signature
/// (u64 LE; constant value = `BLS_SIGNATURE_LEN`).
const SIG_LEN_PREFIX_LEN: usize = 8;

/// Length of a BLS12-381 G2 compressed signature.
const BLS_SIGNATURE_LEN: usize = 192;

/// Absolute offset where the BLS signature bytes begin.
const SIG_OFFSET: usize = SIG_LEN_PREFIX_LEN;

/// Absolute offset where the BLS signature bytes end / `num_signers` begins.
const NUM_SIGNERS_OFFSET: usize = SIG_OFFSET + BLS_SIGNATURE_LEN; // 200

/// Bincode `Vec<…>` length prefix for `signature_occurrences` (u64 LE).
const NUM_SIGNERS_LEN: usize = 8;

/// Absolute offset where the signer-entry array begins.
const SIGNER_ENTRIES_OFFSET: usize = NUM_SIGNERS_OFFSET + NUM_SIGNERS_LEN; // 208

/// Size of one `(SignerIndex, u16)` entry — u16 LE signer index + u16 LE count.
const SIGNER_ENTRY_LEN: usize = 4;

/// Extract the 192-byte BLS signature from the attestation.
pub fn parse_signature_bytes(attestation_bytes: &[u8]) -> &[u8] {
    &attestation_bytes[SIG_OFFSET..NUM_SIGNERS_OFFSET]
}

/// Read the number of signer entries (`signature_occurrences.len()`).
pub fn parse_num_signers(attestation_bytes: &[u8]) -> usize {
    let bytes = &attestation_bytes[NUM_SIGNERS_OFFSET..NUM_SIGNERS_OFFSET + NUM_SIGNERS_LEN];
    u64::from_le_bytes(bytes.try_into().unwrap()) as usize
}

/// Parse `num_signers` × `(u16 signer_idx, u16 count)` entries.
pub fn parse_signer_entries(attestation_bytes: &[u8]) -> Vec<(u16, u16)> {
    let n = parse_num_signers(attestation_bytes);
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let base = SIGNER_ENTRIES_OFFSET + i * SIGNER_ENTRY_LEN;
        let idx = u16::from_le_bytes(attestation_bytes[base..base + 2].try_into().unwrap());
        let count = u16::from_le_bytes(attestation_bytes[base + 2..base + 4].try_into().unwrap());
        entries.push((idx, count));
    }
    entries
}

/// Compute the byte offset where the inner `AttestationData` begins.
pub fn attestation_data_offset(num_signers: usize) -> usize {
    SIGNER_ENTRIES_OFFSET + num_signers * SIGNER_ENTRY_LEN
}

/// Extract the signed message (`AttestationData`) bytes from the attestation.
pub fn parse_attestation_data_bytes(attestation_bytes: &[u8]) -> &[u8] {
    let offset = attestation_data_offset(parse_num_signers(attestation_bytes));
    &attestation_bytes[offset..]
}

/// Extract `block_seq_no` (u32 LE) directly from raw attestation bytes.
pub fn compute_block_seq_no(attestation_bytes: &[u8]) -> u32 {
    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + crate::BLOCK_SEQ_NO_REL_OFFSET;
    let seqno_bytes = &attestation_bytes[abs_offset..abs_offset + 4];
    u32::from_le_bytes(seqno_bytes.try_into().unwrap())
}

/// Extract `block_id` (32 bytes) from the attestation payload and pack into
/// `Fr` as the natural integer value of the big-endian SHA-256 digest.
///
/// AN writes `block_id` verbatim from `Sha256::finalize()` (BE bytes) into
/// `BlockIdentifier` and bincode-serialises it into `AttestationData` as-is
/// (`node/libs/node-types/src/{types.rs,u256.rs}` with `ser = bytes`); when
/// it later publishes the same value externally it does `hex::encode` over
/// those BE bytes (`u256.rs`). To match:
/// - the in-circuit fold in `primary_circuit.rs` / `fallback_circuit.rs`
///   (both `inner_product` a reversed byte iterator);
/// - Circuit 2's `block_id_fr` (folds `reverse(sha256_root)`);
/// - Solidity's `uint256(bytes32(blockId))` — the single `blockId` value
///   `AckiNackiBridge.verifyBlock` feeds to both SNARK verifiers;
///
/// we reverse the 32 payload bytes and then fold LE with powers of 256,
/// giving `Fr = uint256(bytes32(sha256_root))`.
pub fn compute_block_id_fr(attestation_bytes: &[u8]) -> Fr {
    let num_signers = parse_num_signers(attestation_bytes);
    let abs_offset = attestation_data_offset(num_signers) + crate::BLOCK_ID_REL_OFFSET;
    let block_id_bytes = &attestation_bytes[abs_offset..abs_offset + 32];

    let mut result = Fr::zero();
    let mut power = Fr::one();
    let base = Fr::from(256u64);
    for &byte in block_id_bytes.iter().rev() {
        result += Fr::from(byte as u64) * power;
        power *= base;
    }
    result
}
