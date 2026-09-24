//! Unified native Poseidon hashing for the Acki Nacki → Ethereum bridge.
//!
//! This crate is the single source of truth for all off-circuit (native) Poseidon
//! hashing used by bridge circuits, test-data generators, and the prover.
//!
//! Two input encodings:
//! - **Bytes encoding** ([`poseidon_hash_bytes`]): raw bytes split into 31-byte chunks,
//!   zero-padded to 32 bytes, each chunk loaded as an LE Fr element. Equivalent to
//!   `PoseidonSponge::hash_bytes_flat()` in tvm-sdk.
//! - **Fr encoding** ([`poseidon_hash_fr`]): pre-constructed Fr elements fed directly
//!   into the sponge. Used for BK set commitments.

use std::collections::HashMap;

use gosh_bls_verification::helpers::deserialize_g1_pubkey;
use halo2_base::gates::{
    circuit::builder::BaseCircuitBuilder, GateInstructions, RangeChip, RangeInstructions,
};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use halo2_base::poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher};
use halo2_base::utils::BigPrimeField;
use halo2_base::AssignedValue;
use halo2_ecc::bigint::ProperCrtUint;
use halo2_ecc::ecc::EcPoint;
use num_bigint::BigUint;
use pse_poseidon::Poseidon;

// ---------------------------------------------------------------------------
// Poseidon sponge parameters (BN254-compatible)
// ---------------------------------------------------------------------------

pub const POSEIDON_T: usize = 3;
pub const POSEIDON_RATE: usize = 2;
pub const POSEIDON_R_F: usize = 8;
pub const POSEIDON_R_P: usize = 57;

// ---------------------------------------------------------------------------
// BK set commitment parameters
// ---------------------------------------------------------------------------

/// CRT limb size for BLS12-381 x-coordinate decomposition.
pub const LIMB_BITS: usize = 104;
/// Number of CRT limbs (5 × 104 = 520 bits covers the 381-bit x-coordinate).
pub const NUM_LIMBS: usize = 5;
/// Maximum number of signers in a BK set (padded for fixed circuit structure).
pub const MAX_SIGNERS: usize = 300;
/// Sentinel signer index used for padding entries.
pub const PADDING_SIGNER_INDEX: u16 = 0xFFFF;

// ---------------------------------------------------------------------------
// Layer hashes constants
// ---------------------------------------------------------------------------

/// Maximum number of layers in the layer_hashes preimage.
pub const MAX_LAYERS: usize = 10;
/// Size of the layer_hashes preimage: 1 byte (num_layers) + 10 × 33 bytes.
pub const LAYER_PREIMAGE_SIZE: usize = 1 + MAX_LAYERS * 33; // 331

/// Number of field elements per pubkey in the Poseidon input:
/// 1 (signer index) + num_limbs (x-coordinate CRT limbs).
pub fn poseidon_elements_per_pubkey(num_limbs: usize) -> usize {
    1 + num_limbs
}

// ---------------------------------------------------------------------------
// Core hash functions
// ---------------------------------------------------------------------------

/// Poseidon hash of raw bytes, split into 31-byte chunks.
///
/// Equivalent to `PoseidonSponge::hash_bytes_flat()` from tvm-sdk:
/// 1. Split `data` into chunks of 31 bytes
/// 2. Zero-pad each chunk to 32 bytes
/// 3. Convert each to Fr via `Fr::from_repr` (little-endian)
/// 4. Feed all Fr elements into the Poseidon sponge (T=3, RATE=2, R_F=8, R_P=57)
/// 5. Return `squeeze().to_repr()` (32 bytes LE)
pub fn poseidon_hash_bytes(data: &[u8]) -> [u8; 32] {
    let num_chunks = (data.len() + 30) / 31; // ceil division
    let mut elements: Vec<Fr> = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let start = i * 31;
        let end = std::cmp::min(start + 31, data.len());
        let chunk = &data[start..end];
        let mut buf = [0u8; 32];
        buf[..chunk.len()].copy_from_slice(chunk);
        elements.push(Fr::from_repr(buf).unwrap());
    }
    let mut sponge = Poseidon::<Fr, POSEIDON_T, POSEIDON_RATE>::new(POSEIDON_R_F, POSEIDON_R_P);
    sponge.update(&elements);
    sponge.squeeze().to_repr()
}

/// Poseidon hash of Fr elements, returning the Fr result.
pub fn poseidon_hash_fr(inputs: &[Fr]) -> Fr {
    let mut sponge = Poseidon::<Fr, POSEIDON_T, POSEIDON_RATE>::new(POSEIDON_R_F, POSEIDON_R_P);
    sponge.update(inputs);
    sponge.squeeze()
}

/// Poseidon hash of Fr elements, returning 32-byte LE representation.
pub fn poseidon_hash_fr_to_bytes(inputs: &[Fr]) -> [u8; 32] {
    poseidon_hash_fr(inputs).to_repr()
}

// ---------------------------------------------------------------------------
// BLS pubkey x-coordinate decomposition
// ---------------------------------------------------------------------------

/// Decompose a compressed BLS12-381 G1 public key into CRT limbs.
///
/// Steps:
/// 1. Decompress 48-byte pubkey → G1Affine
/// 2. Extract x-coordinate: `g1.x.to_bytes()` → 48 bytes LE
/// 3. Convert to BigUint
/// 4. Split into 5 limbs of 104 bits each
/// 5. Each limb → BN254 Fr element
pub fn decompose_pubkey_x_to_limbs(pubkey_bytes: &[u8]) -> [Fr; NUM_LIMBS] {
    let g1 = deserialize_g1_pubkey(pubkey_bytes);

    let x_bytes_le = g1.x.to_bytes();
    let x_bigint = BigUint::from_bytes_le(&x_bytes_le);

    let limb_mask = (BigUint::from(1u64) << LIMB_BITS) - 1u64;
    let mut limbs = [Fr::zero(); NUM_LIMBS];

    for i in 0..NUM_LIMBS {
        let limb_val = (&x_bigint >> (i * LIMB_BITS)) & &limb_mask;
        let limb_bytes = limb_val.to_bytes_le();
        let mut buf = [0u8; 32];
        let len = limb_bytes.len().min(32);
        buf[..len].copy_from_slice(&limb_bytes[..len]);
        limbs[i] = Fr::from_repr(buf).unwrap();
    }

    limbs
}

// ---------------------------------------------------------------------------
// BK set Poseidon commitment
// ---------------------------------------------------------------------------

/// Compute Poseidon commitment of a BK set, padded to `max_signers` entries.
///
/// Real entries use pubkey x-coordinate CRT limbs; padding entries
/// (when `bk_set.len() < max_signers`) use sentinel index `PADDING_SIGNER_INDEX`
/// + zero x-limbs.
///
/// Input: `bk_set` maps signer_index (u16) → compressed BLS pubkey (48 bytes).
///
/// Returns `(Fr_result, 32-byte LE repr)`.
///
/// **Parity warning:** acki-nacki's `BlockKeeperSet::poseidon_commitment()`
/// hard-codes `max_signers = 300`, so this function only matches it when
/// called with `max_signers == MAX_SIGNERS` (=300). For other values the
/// result is a valid bridge-side commitment but will not agree with the
/// chain. See `README.md`.
pub fn compute_bk_set_poseidon_padded_to(
    bk_set: &HashMap<u16, Vec<u8>>,
    max_signers: usize,
) -> (Fr, [u8; 32]) {
    let mut sorted_keys: Vec<u16> = bk_set.keys().cloned().collect();
    sorted_keys.sort();
    let actual_size = sorted_keys.len();

    let limb_mask = (BigUint::from(1u64) << LIMB_BITS) - 1u64;
    let mut poseidon_input: Vec<Fr> = Vec::with_capacity(max_signers * (1 + NUM_LIMBS));

    for k in 0..max_signers {
        if k < actual_size {
            let idx = sorted_keys[k];
            poseidon_input.push(Fr::from(idx as u64));

            let pk_bytes = &bk_set[&idx];
            let g1 = deserialize_g1_pubkey(pk_bytes);
            let x_bytes_le = g1.x.to_bytes();
            let x_bigint = BigUint::from_bytes_le(&x_bytes_le);

            for i in 0..NUM_LIMBS {
                let limb_val = (&x_bigint >> (i * LIMB_BITS)) & &limb_mask;
                let limb_bytes = limb_val.to_bytes_le();
                let mut buf = [0u8; 32];
                let len = limb_bytes.len().min(32);
                buf[..len].copy_from_slice(&limb_bytes[..len]);
                poseidon_input.push(Fr::from_repr(buf).unwrap());
            }
        } else {
            poseidon_input.push(Fr::from(PADDING_SIGNER_INDEX as u64));
            for _ in 0..NUM_LIMBS {
                poseidon_input.push(Fr::zero());
            }
        }
    }

    let mut sponge = Poseidon::<Fr, POSEIDON_T, POSEIDON_RATE>::new(POSEIDON_R_F, POSEIDON_R_P);
    sponge.update(&poseidon_input);
    let result = sponge.squeeze();
    (result, result.to_repr())
}

/// Compute Poseidon commitment of a BK set padded to `MAX_SIGNERS` (=300) —
/// the canonical bridge production hash that matches acki-nacki's
/// `BlockKeeperSet::poseidon_commitment()`.
///
/// Thin wrapper over [`compute_bk_set_poseidon_padded_to`]. Use this from
/// non-test code (bridge prover, daemons). Tests that exercise alternative
/// padding sizes should call the parameterized variant directly.
pub fn compute_bk_set_poseidon(bk_set: &HashMap<u16, Vec<u8>>) -> (Fr, [u8; 32]) {
    compute_bk_set_poseidon_padded_to(bk_set, MAX_SIGNERS)
}

// ---------------------------------------------------------------------------
// In-circuit BK set Poseidon commitment
// ---------------------------------------------------------------------------

/// In-circuit Poseidon commitment to a BK set using EC point x-coordinates.
///
/// `assigned_pks` and `sorted_bk_set_indices` are always padded to `assigned_pks.len()`
/// (typically `MAX_SIGNERS`). Real entries (k < `actual_bk_set_size`) use the EC
/// point's x-coordinate CRT limbs. Padding entries (k >= `actual_bk_set_size`) use
/// zero x-limbs and sentinel index `PADDING_SIGNER_INDEX` (`0xFFFF`).
///
/// The Poseidon input is always `assigned_pks.len() × (1 + num_limbs)` elements,
/// ensuring a fixed circuit structure regardless of actual BK set size.
///
/// **Single-VK support:** All signer indices are loaded as witnesses (not constants),
/// and limb selection uses `gate.mul(limb, is_real)` for all entries. The
/// copy-constraint pattern is identical regardless of `actual_bk_set_size`, so a
/// single VK/PK works for any BK set size up to `assigned_pks.len()`.
///
/// Soundness: the Poseidon output must match the public instance, which forces
/// the prover to use correct index and limb values (Poseidon collision resistance).
///
/// Returns `(commitment, n_real_pubkeys)` where `n_real_pubkeys` is the in-circuit
/// count of real (non-padding) entries, for use in threshold checks.
pub fn compute_bk_set_commitment_padded<F: BigPrimeField>(
    builder: &mut BaseCircuitBuilder<F>,
    range: &RangeChip<F>,
    assigned_pks: &[EcPoint<F, ProperCrtUint<F>>],
    sorted_bk_set_indices: &[u16],
    num_limbs: usize,
    actual_bk_set_size: usize,
) -> (AssignedValue<F>, AssignedValue<F>) {
    assert_eq!(
        assigned_pks.len(),
        sorted_bk_set_indices.len(),
        "assigned_pks and sorted_bk_set_indices must be padded to the same length"
    );

    let ctx = builder.main(0);
    let gate = range.gate();

    let spec = OptimizedPoseidonSpec::<F, POSEIDON_T, POSEIDON_RATE>::new::<
        POSEIDON_R_F,
        POSEIDON_R_P,
        0,
    >();
    let mut poseidon = PoseidonHasher::<F, POSEIDON_T, POSEIDON_RATE>::new(spec);
    poseidon.initialize_consts(ctx, gate);

    let elems_per_pk = poseidon_elements_per_pubkey(num_limbs);
    let mut poseidon_input: Vec<AssignedValue<F>> =
        Vec::with_capacity(assigned_pks.len() * elems_per_pk);

    // Accumulate n_real_pubkeys = sum(is_real) for threshold checks.
    let mut n_real = ctx.load_constant(F::ZERO);

    for k in 0..assigned_pks.len() {
        // is_real: 1 for real entries, 0 for padding (witness, bit-constrained).
        let is_real_val = if k < actual_bk_set_size { F::ONE } else { F::ZERO };
        let is_real = ctx.load_witness(is_real_val);
        gate.assert_bit(ctx, is_real);
        n_real = gate.add(ctx, n_real, is_real);

        // Index: loaded as witness for single-VK support.
        let idx_cell = ctx.load_witness(F::from(sorted_bk_set_indices[k] as u64));
        poseidon_input.push(idx_cell);

        // Limbs: multiply by is_real to zero out padding entries.
        // Real entries: limb * 1 = limb.  Padding: limb * 0 = 0.
        let x_limbs = assigned_pks[k].x().limbs();
        for &limb in x_limbs {
            let selected = gate.mul(ctx, limb, is_real);
            poseidon_input.push(selected);
        }
    }

    let commitment = poseidon.hash_fix_len_array(ctx, gate, &poseidon_input);
    (commitment, n_real)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poseidon_hash_bytes_deterministic() {
        let data = vec![1u8; 331];
        let h1 = poseidon_hash_bytes(&data);
        let h2 = poseidon_hash_bytes(&data);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn test_poseidon_hash_bytes_331() {
        // 331 bytes -> ceil(331/31) = 11 chunks
        let data = vec![0x42u8; 331];
        let h = poseidon_hash_bytes(&data);
        assert_ne!(h, [0u8; 32]);
    }

    #[test]
    fn test_poseidon_hash_bytes_empty_chunks() {
        // 31 bytes = exactly 1 chunk
        let h1 = poseidon_hash_bytes(&[0xAA; 31]);
        // 32 bytes = 2 chunks (31 + 1)
        let h2 = poseidon_hash_bytes(&[0xAA; 32]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_poseidon_hash_fr_deterministic() {
        let inputs = vec![Fr::from(42u64), Fr::from(123u64)];
        let h1 = poseidon_hash_fr(&inputs);
        let h2 = poseidon_hash_fr(&inputs);
        assert_eq!(h1, h2);
        assert_ne!(h1, Fr::zero());
    }

    #[test]
    fn test_poseidon_hash_fr_to_bytes_matches() {
        let inputs = vec![Fr::from(42u64), Fr::from(123u64)];
        let fr_result = poseidon_hash_fr(&inputs);
        let bytes_result = poseidon_hash_fr_to_bytes(&inputs);
        assert_eq!(fr_result.to_repr(), bytes_result);
    }

    #[test]
    fn test_poseidon_hash_bytes_64_three_chunks() {
        // 64 bytes -> 3 chunks: [0..31], [31..62], [62..64]
        // This is the encoding used for Poseidon Merkle tree internal nodes
        let left = [0xAAu8; 32];
        let right = [0xBBu8; 32];
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&left);
        concat[32..].copy_from_slice(&right);
        let h = poseidon_hash_bytes(&concat);
        assert_ne!(h, [0u8; 32]);

        // Order matters
        let mut concat2 = [0u8; 64];
        concat2[..32].copy_from_slice(&right);
        concat2[32..].copy_from_slice(&left);
        let h2 = poseidon_hash_bytes(&concat2);
        assert_ne!(h, h2);
    }

    #[test]
    fn test_compute_bk_set_poseidon_empty() {
        let bk_set: HashMap<u16, Vec<u8>> = HashMap::new();
        let (fr, bytes) = compute_bk_set_poseidon(&bk_set);
        // Empty set with all 300 padding entries should still produce a valid hash
        assert_ne!(fr, Fr::zero());
        assert_eq!(fr.to_repr(), bytes);
    }

    #[test]
    fn test_compute_bk_set_poseidon_deterministic() {
        // Can't easily construct a real BLS keypair without gosh_blst,
        // but determinism is testable with the empty set
        let bk_set: HashMap<u16, Vec<u8>> = HashMap::new();
        let (fr1, _) = compute_bk_set_poseidon(&bk_set);
        let (fr2, _) = compute_bk_set_poseidon(&bk_set);
        assert_eq!(fr1, fr2);
    }

    #[test]
    fn test_poseidon_hash_bytes_matches_manual_fr() {
        // Verify that poseidon_hash_bytes over 64 bytes produces the same
        // result as manually chunking and calling poseidon_hash_fr
        let data = [0x42u8; 64];

        let hash_bytes_result = poseidon_hash_bytes(&data);

        // Manual chunking: 31 + 31 + 2
        let mut buf0 = [0u8; 32];
        buf0[..31].copy_from_slice(&data[0..31]);
        let mut buf1 = [0u8; 32];
        buf1[..31].copy_from_slice(&data[31..62]);
        let mut buf2 = [0u8; 32];
        buf2[..2].copy_from_slice(&data[62..64]);

        let elements = vec![
            Fr::from_repr(buf0).unwrap(),
            Fr::from_repr(buf1).unwrap(),
            Fr::from_repr(buf2).unwrap(),
        ];
        let manual_result = poseidon_hash_fr_to_bytes(&elements);

        assert_eq!(hash_bytes_result, manual_result);
    }
}
