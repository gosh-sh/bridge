//! M1 — BLS core for the Ethereum sync-committee light client.
//!
//! This is a thin **Ethereum-specific wrapper** over the already-audited
//! committee-BLS-aggregate gadget in `gosh-bls-verification` (used by the AN→ETH
//! Circuit 1A) plus the RFC-9380 hash-to-curve chip in `halo2-ecc::bls12_381`.
//! We do **not** re-implement pairing / MSM / threshold logic; we only add the
//! glue that differs between GoshBLS (Acki Nacki) and Ethereum consensus:
//!
//! | Aspect                | GoshBLS (AN)                     | Ethereum sync committee (here) |
//! |-----------------------|----------------------------------|--------------------------------|
//! | hash-to-curve DST     | `..._SSWU_RO_NUL_`               | `..._SSWU_RO_POP_`             |
//! | signature encoding    | 192 B uncompressed G2            | 96 B compressed G2             |
//! | signer selection      | `(index, count)` list            | 512-bit `sync_committee_bits`  |
//! | supermajority         | Primary `3n >= 2N`               | same (2/3 of 512 = 342)        |
//! | committee size `N`    | dynamic (`actual_num_pubkeys`)   | fixed 512                      |
//!
//! The signed message (`signing_root`) is produced by SSZ merkleization in M2;
//! in M1 it is supplied as a 32-byte witness and either (a) loaded as a private
//! G2 point after off-circuit hash-to-curve, or (b) hashed to G2 **in-circuit**
//! via [`assign_signing_root_hash_to_g2`] (the eventual production path).
//!
//! ## Trust / soundness notes (see `docs/m1_notes.md`)
//! - **G1 pubkeys**: `load_bk_set_pubkeys` adds an on-curve check. Subgroup
//!   membership is transitively trusted via the SSZ binding of the 512 pubkeys
//!   to the anchored `active_sync_committee_root` (wired in M2).
//! - **G2 signature**: `verify_bls_attestation_with_assigned_msghash` adds an
//!   on-curve check but **not** a subgroup check (audit BLS-1 / FORK-2). M1 ships
//!   an off-circuit guard ([`assert_witness_subgroup`]). **Resolved in-circuit**
//!   by [`crate::subgroup`] (ψ-endomorphism check), which the M3 step circuit
//!   ([`crate::step`]) uses on the *same* assigned signature cell that feeds the
//!   pairing — so the M3 path no longer relies on the off-circuit guard.

use gosh_bls_verification::{
    compute_all_pub_sum, load_bk_set_pubkeys, verify_bls_attestation_with_assigned_msghash,
    ThresholdMode,
};
use halo2_base::gates::flex_gate::threads::SinglePhaseCoreManager;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1Affine, G2Affine};
use halo2_base::halo2_proofs::halo2curves::group::Curve;
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, QuantumCell};
use halo2_ecc::bls12_381::{Fp2Chip, FpChip, G2Point};
use halo2_ecc::ecc::hash_to_curve::{ExpandMsgXmd, HashInstructions, HashToCurveChip};

// ---------------------------------------------------------------------------
// Ethereum mainnet-preset constants (see docs/m0_spec.md §5)
// ---------------------------------------------------------------------------

/// Number of validators in a sync committee (mainnet preset).
pub const SYNC_COMMITTEE_SIZE: usize = 512;

/// Our participation policy: supermajority `ceil(2*512/3) = 342`.
/// Enforced in-circuit by [`ThresholdMode::Primary`] (`3*n_signers >= 2*N`).
pub const SYNC_COMMITTEE_THRESHOLD: usize = 342;

/// Domain separation tag for BLS signatures on Ethereum (proof-of-possession
/// ciphersuite). Note the `POP_` suffix — GoshBLS uses `NUL_`.
pub const ETH_BLS_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

/// halo2-ecc non-native field decomposition for BLS12-381 `Fq` over BN254.
/// Matches the gosh reference tests (`limb_bits = 104`, `num_limbs = 5`).
pub const LIMB_BITS: usize = 104;
pub const NUM_LIMBS: usize = 5;

// ---------------------------------------------------------------------------
// Off-circuit helpers (witness preparation + native sanity checks)
// ---------------------------------------------------------------------------

/// Convert a 512-bit `sync_committee_bits` mask into the `(index, count)` signer
/// list expected by `verify_bls_attestation_with_assigned_msghash`. Every
/// participant contributes with multiplicity 1 (Ethereum has no repeated signers).
pub fn bits_to_signers_data(bits: &[bool]) -> Vec<(u16, u16)> {
    assert_eq!(
        bits.len(),
        SYNC_COMMITTEE_SIZE,
        "sync_committee_bits must be exactly {SYNC_COMMITTEE_SIZE} bits"
    );
    bits.iter()
        .enumerate()
        .filter(|(_, &b)| b)
        .map(|(i, _)| (i as u16, 1u16))
        .collect()
}

/// popcount of the participation bitmask.
pub fn participation(bits: &[bool]) -> usize {
    bits.iter().filter(|&&b| b).count()
}

/// Decode a big-endian bitfield (`sync_committee_bits`, 64 bytes LE-byte /
/// bit-index order per SSZ `Bitvector[512]`) into 512 booleans.
///
/// SSZ `Bitvector` packs bit `i` into byte `i/8`, bit position `i%8` (LSB-first).
pub fn decode_sync_committee_bits(bytes: &[u8]) -> Vec<bool> {
    assert_eq!(bytes.len(), SYNC_COMMITTEE_SIZE / 8, "expected 64 bytes");
    (0..SYNC_COMMITTEE_SIZE)
        .map(|i| (bytes[i / 8] >> (i % 8)) & 1 == 1)
        .collect()
}

/// Deserialize a 48-byte compressed BLS public key (Ethereum, big-endian ZCash
/// format). Reuses the gosh helper (tries BE then LE).
pub fn deserialize_pubkey(pk_bytes: &[u8]) -> G1Affine {
    gosh_bls_verification::helpers::deserialize_g1_pubkey(pk_bytes)
}

/// Deserialize a 96-byte **compressed** G2 signature (Ethereum wire format).
/// (GoshBLS ships 192-byte uncompressed; Ethereum uses 96-byte compressed.)
pub fn deserialize_signature_compressed(sig_bytes: &[u8]) -> G2Affine {
    assert_eq!(sig_bytes.len(), 96, "BLS signature must be 96 bytes (compressed G2)");
    let bytes: [u8; 96] = sig_bytes.try_into().unwrap();

    let be = G2Affine::from_compressed_be(&bytes);
    if bool::from(be.is_some()) {
        return be.unwrap();
    }
    let le = G2Affine::from_compressed_le(&bytes);
    if bool::from(le.is_some()) {
        return le.unwrap();
    }
    panic!(
        "failed to decode 96-byte compressed G2 signature (tried BE and LE); first 8 bytes: {:02x?}",
        &sig_bytes[..8]
    );
}

/// Native RFC-9380 hash-to-curve of the `signing_root` to G2 with the Ethereum
/// `POP_` DST. Used to (a) build the message-hash witness for the fast M1 test
/// path, and (b) cross-check the in-circuit hash-to-curve output.
pub fn signing_root_to_g2(signing_root: &[u8; 32]) -> G2Affine {
    use halo2_base::halo2_proofs::halo2curves::bls12_381::hash_to_curve::{
        ExpandMsgXmd as NativeExpandMsgXmd, HashToCurve,
    };
    use halo2_base::halo2_proofs::halo2curves::bls12_381::G2;
    use sha2::Sha256;
    <G2 as HashToCurve<NativeExpandMsgXmd<Sha256>>>::hash_to_curve(signing_root, ETH_BLS_DST)
        .to_affine()
}

/// Native pairing check `e(-G1, sig) * e(agg_pk, H(m)) == 1`, for off-circuit
/// witness validation before proving. Delegates to the gosh helper.
pub fn verify_native(signature: &G2Affine, agg_pubkey: &G1Affine, msg_hash: &G2Affine) -> bool {
    gosh_bls_verification::helpers::verify_bls_native(signature, agg_pubkey, msg_hash)
}

/// Off-circuit guard: witness points must be torsion-free (in the prime-order
/// subgroup). Run this on the committee pubkeys and the signature **before**
/// proving; the in-circuit path does not yet enforce G2 subgroup membership
/// (see BLS-1 note in the module docs / `m1_notes.md`).
pub fn assert_witness_subgroup(pubkeys: &[G1Affine], signature: &G2Affine) {
    for (i, pk) in pubkeys.iter().enumerate() {
        assert!(
            bool::from(pk.is_torsion_free()),
            "committee pubkey #{i} is not in the G1 prime-order subgroup"
        );
    }
    assert!(
        bool::from(signature.is_torsion_free()),
        "signature is not in the G2 prime-order subgroup"
    );
}

// ---------------------------------------------------------------------------
// In-circuit hash-to-curve (RFC 9380, SHA-256 XMD, SSWU -> G2)
// ---------------------------------------------------------------------------

/// Hash the 32-byte `signing_root` to a G2 point in-circuit with the Ethereum
/// `POP_` DST, returning the assigned point to feed into [`verify_sync_aggregate`].
///
/// Generic over the SHA-256 chip `HC`: in M1 tests we pass a mock (off-circuit
/// digest) to exercise the wiring; M2 swaps in the real `gosh-sha256-chip` so the
/// digest is itself constrained.
pub fn assign_signing_root_hash_to_g2<F, HC>(
    pool: &mut SinglePhaseCoreManager<F>,
    range: &RangeChip<F>,
    hash_chip: &HC,
    signing_root: &[u8; 32],
) -> G2Point<F>
where
    F: BigPrimeField,
    HC: HashInstructions<F, CircuitBuilder = SinglePhaseCoreManager<F>>,
{
    let fp_chip = FpChip::<F>::new(range, LIMB_BITS, NUM_LIMBS);
    let fp2_chip = Fp2Chip::new(&fp_chip);
    let h2c_chip = HashToCurveChip::new(hash_chip, &fp2_chip);

    let assigned_msg: Vec<AssignedValue<F>> = signing_root
        .iter()
        .map(|&b| pool.main().load_witness(F::from(b as u64)))
        .collect();

    h2c_chip
        .hash_to_curve::<ExpandMsgXmd>(
            pool,
            assigned_msg.into_iter().map(QuantumCell::Existing),
            ETH_BLS_DST,
        )
        .expect("in-circuit hash-to-curve failed")
}

// ---------------------------------------------------------------------------
// In-circuit sync-committee aggregate verification (the M1 deliverable)
// ---------------------------------------------------------------------------

/// Verify a sync-committee `SyncAggregate` in-circuit:
/// aggregate the participating committee pubkeys (selected by `bits`), enforce
/// the 2/3 supermajority, and check the BLS pairing against `msghash_assigned`
/// (the G2 hash of the beacon `signing_root`).
///
/// `committee_pubkeys` are the 512 active-committee G1 pubkeys (their SSZ binding
/// to `active_sync_committee_root` is enforced in M2). `msghash_assigned` comes
/// from [`assign_signing_root_hash_to_g2`] (production) or a private G2 load of
/// [`signing_root_to_g2`] (fast test path).
///
/// Constraints delegated to `gosh-bls-verification`:
/// on-curve(G1 pubkeys), on-curve(G2 sig), supermajority `3n >= 2*512`,
/// strictly-increasing distinct signer indices, aggregate = MSM(pk, weights),
/// and `e(-G1, sig) * e(agg_pk, msghash) == 1`.
pub fn verify_sync_aggregate<F: BigPrimeField>(
    pool: &mut SinglePhaseCoreManager<F>,
    range: &RangeChip<F>,
    committee_pubkeys: &[G1Affine],
    bits: &[bool],
    signature: G2Affine,
    msghash_assigned: G2Point<F>,
) {
    assert_eq!(
        committee_pubkeys.len(),
        SYNC_COMMITTEE_SIZE,
        "expected exactly {SYNC_COMMITTEE_SIZE} committee pubkeys"
    );
    let signers_data = bits_to_signers_data(bits);
    assert!(
        signers_data.len() >= SYNC_COMMITTEE_THRESHOLD,
        "participation {} below supermajority threshold {}",
        signers_data.len(),
        SYNC_COMMITTEE_THRESHOLD
    );

    // Phase 1 (single-threaded): load & on-curve-check the committee pubkeys,
    // precompute their sum (MSM shift correction), and pin the committee size.
    let (assigned_pks, all_pub_sum, actual_npk) = {
        let ctx = pool.main();
        let assigned_pks = load_bk_set_pubkeys(ctx, range, committee_pubkeys, LIMB_BITS, NUM_LIMBS);
        let all_pub_sum = compute_all_pub_sum(ctx, range, &assigned_pks, LIMB_BITS, NUM_LIMBS);
        let actual_npk = ctx.load_witness(F::from(SYNC_COMMITTEE_SIZE as u64));
        (assigned_pks, all_pub_sum, actual_npk)
    };

    verify_bls_attestation_with_assigned_msghash(
        pool,
        range,
        signature,
        msghash_assigned,
        &assigned_pks,
        &signers_data,
        SYNC_COMMITTEE_SIZE, // max_signers capacity
        LIMB_BITS,
        NUM_LIMBS,
        ThresholdMode::Primary, // 3n >= 2N  <=>  n >= 342
        all_pub_sum,
        actual_npk,
    );
}

// ---------------------------------------------------------------------------
// Off-circuit unit tests (no MockProver — cheap to run once the crate compiles)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1, G2};
    use halo2_base::halo2_proofs::halo2curves::group::Group;

    #[test]
    fn threshold_is_two_thirds() {
        // Primary formula `3n >= 2N` with N = 512 must fire exactly at 342.
        assert_eq!(SYNC_COMMITTEE_THRESHOLD, 342);
        assert!(3 * 342 >= 2 * SYNC_COMMITTEE_SIZE);
        assert!(3 * 341 < 2 * SYNC_COMMITTEE_SIZE);
    }

    #[test]
    fn eth_dst_uses_pop_not_nul() {
        assert!(ETH_BLS_DST.ends_with(b"_POP_"));
        assert_ne!(ETH_BLS_DST, gosh_bls_verification::helpers::DST);
    }

    #[test]
    fn bits_roundtrip_and_signers() {
        let mut bits = vec![false; SYNC_COMMITTEE_SIZE];
        bits[0] = true;
        bits[7] = true;
        bits[511] = true;
        assert_eq!(participation(&bits), 3);
        let signers = bits_to_signers_data(&bits);
        assert_eq!(signers, vec![(0, 1), (7, 1), (511, 1)]);
    }

    #[test]
    fn decode_bits_ssz_lsb_first() {
        // byte 0 = 0b1000_0001 -> bits 0 and 7 set.
        let mut bytes = vec![0u8; SYNC_COMMITTEE_SIZE / 8];
        bytes[0] = 0b1000_0001;
        bytes[63] = 0b0000_0001; // bit 504
        let bits = decode_sync_committee_bits(&bytes);
        assert!(bits[0] && bits[7] && bits[504]);
        assert_eq!(participation(&bits), 3);
    }

    #[test]
    fn native_aggregate_verifies() {
        use halo2_base::halo2_proofs::halo2curves::ff::Field;
        use rand::rngs::OsRng;

        let signing_root = [7u8; 32];
        let msg_hash = signing_root_to_g2(&signing_root);

        // 4 signers, sk_i random; agg_pk = Σ pk_i, sig = Σ sk_i·H(m).
        let sks: Vec<_> = (0..4)
            .map(|_| <G1 as Group>::Scalar::random(OsRng))
            .collect();
        let mut agg_pk = G1::identity();
        let mut sig = G2::identity();
        let hm = G2::from(msg_hash);
        for sk in &sks {
            agg_pk += G1::generator() * sk;
            sig += hm * sk;
        }
        let agg_pk = agg_pk.to_affine();
        let signature = sig.to_affine();

        assert!(verify_native(&signature, &agg_pk, &msg_hash));
        // Wrong message must fail.
        let bad = signing_root_to_g2(&[9u8; 32]);
        assert!(!verify_native(&signature, &agg_pk, &bad));
        // Generated points are in-subgroup.
        assert_witness_subgroup(&[agg_pk], &signature);
    }
}
