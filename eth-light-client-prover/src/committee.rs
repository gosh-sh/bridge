//! M2 — `hash_tree_root(SyncCommittee)`: binds the 512 committee G1 pubkeys
//! (the ones M1 aggregates) + `aggregate_pubkey` to `active_sync_committee_root`.
//!
//! `SyncCommittee` is a container of two fields:
//! - `pubkeys: Vector[Bytes48, 512]` → `merkleize([htr(pk_i) for i in 0..512])`
//!   (512 is already a power of two, depth 9);
//! - `aggregate_pubkey: Bytes48` → `htr(Bytes48)`.
//!
//! `htr(Bytes48)` = `SHA256(pk[0..32] ‖ (pk[32..48] ‖ 0^16))` (two 32-byte chunks).
//!
//! **M2→M3 seam:** this takes the pubkeys as **48-byte compressed witnesses**. The
//! constraint that these bytes decode to the exact G1 points M1 aggregates (compressed
//! point decode: big-endian x + sign/inf flag bits, on-curve) is stitched in M3, where
//! the full update circuit couples M1 (pairing) and M2 (SSZ). Off-circuit, callers must
//! ensure `deserialize_pubkey(bytes) == g1_affine` (helper below).

use crate::ssz::{bytes_root, container_root, merkleize, native_bytes_root, native_container_root,
    native_merkleize, Node};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, Context};

/// Mainnet sync-committee size.
pub const SYNC_COMMITTEE_SIZE: usize = 512;

/// `hash_tree_root(SyncCommittee)` from 512 compressed pubkeys + aggregate.
///
/// `pubkeys` is a flat `512 * 48` byte-witness slice; `aggregate` is 48 bytes.
pub fn sync_committee_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    pubkeys: &[AssignedValue<F>],
    aggregate: &[AssignedValue<F>],
) -> Node<F> {
    assert_eq!(pubkeys.len(), SYNC_COMMITTEE_SIZE * 48, "expected 512*48 pubkey bytes");
    assert_eq!(aggregate.len(), 48, "aggregate_pubkey must be 48 bytes");

    let pubkey_roots: Vec<Node<F>> =
        pubkeys.chunks(48).map(|pk| bytes_root(chip, ctx, pk)).collect();
    let pubkeys_vector_root = merkleize(chip, ctx, pubkey_roots);
    let aggregate_root = bytes_root(chip, ctx, aggregate);
    container_root(chip, ctx, vec![pubkeys_vector_root, aggregate_root])
}

// ---------------------------------------------------------------------------
// Sharded committee root (M6 recursive rotate)
// ---------------------------------------------------------------------------
//
// `sync_committee_root` is ~1023 in-circuit SHA-256 (512 leaf hashes + 511 tree
// nodes + aggregate + container). Its *assignment* exceeds a 125 GB host at the
// k≈26 it needs, so it cannot be proven monolithically on our hardware. Because
// `pubkeys_vector_root = merkleize(512 leaves)` is a **balanced** binary tree, it
// factors exactly into N contiguous subtrees: `merkleize(512) ==
// merkleize([subtree_root_j for j in 0..N])`. So each shard proves one subtree
// (small k, fits comfortably) and a cheap aggregation snark-verifies the shards
// and finishes the top few levels + aggregate + container. See
// `docs/m6_rotate_recursive.md`.

/// How many shard proofs the committee SHA root is split into. `SYNC_COMMITTEE_SIZE
/// / COMMITTEE_SHARDS` must be a power of two so each shard is a whole balanced
/// subtree. 8 shards → 64 pubkeys/shard → ~127 SHA-256/shard (depth-6 subtree).
pub const COMMITTEE_SHARDS: usize = 8;

/// Pubkeys per shard (`64` for the default 8-shard split).
pub const PUBKEYS_PER_SHARD: usize = SYNC_COMMITTEE_SIZE / COMMITTEE_SHARDS;

/// In-circuit subtree root of one contiguous shard of pubkeys:
/// `merkleize([bytes_root(pk_i) for pk_i in slice])`. `pubkey_slice` is `n*48`
/// bytes with `n` a power of two. This binds the shard's pubkey byte-witnesses to
/// a single 32-byte root the shard proof exposes as its public output.
pub fn committee_subtree_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    pubkey_slice: &[AssignedValue<F>],
) -> Node<F> {
    assert!(pubkey_slice.len() % 48 == 0, "shard must be a whole number of 48-byte pubkeys");
    let n = pubkey_slice.len() / 48;
    assert!(n.is_power_of_two(), "pubkeys-per-shard must be a power of two");
    let roots: Vec<Node<F>> = pubkey_slice.chunks(48).map(|pk| bytes_root(chip, ctx, pk)).collect();
    merkleize(chip, ctx, roots)
}

/// Recompose the full `hash_tree_root(SyncCommittee)` from the N shard subtree
/// roots + `aggregate_pubkey`. `subtree_roots.len()` must be a power of two (=
/// `COMMITTEE_SHARDS`). This is the cheap top the aggregation circuit runs after
/// snark-verifying the shards — a handful of SHA-256, so it fits any k.
pub fn sync_committee_root_from_subtrees<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    subtree_roots: Vec<Node<F>>,
    aggregate: &[AssignedValue<F>],
) -> Node<F> {
    assert!(subtree_roots.len().is_power_of_two(), "shard count must be a power of two");
    assert_eq!(aggregate.len(), 48, "aggregate_pubkey must be 48 bytes");
    let pubkeys_vector_root = merkleize(chip, ctx, subtree_roots);
    let aggregate_root = bytes_root(chip, ctx, aggregate);
    container_root(chip, ctx, vec![pubkeys_vector_root, aggregate_root])
}

// ---------------------------------------------------------------------------
// Native reference
// ---------------------------------------------------------------------------

pub fn native_sync_committee_root(pubkeys: &[[u8; 48]], aggregate: &[u8; 48]) -> [u8; 32] {
    assert_eq!(pubkeys.len(), SYNC_COMMITTEE_SIZE);
    let pubkey_roots: Vec<[u8; 32]> = pubkeys.iter().map(|pk| native_bytes_root(pk)).collect();
    let pubkeys_vector_root = native_merkleize(pubkey_roots);
    let aggregate_root = native_bytes_root(aggregate);
    native_container_root(vec![pubkeys_vector_root, aggregate_root])
}

/// Native twin of [`committee_subtree_root`].
pub fn native_committee_subtree_root(pubkeys: &[[u8; 48]]) -> [u8; 32] {
    assert!(pubkeys.len().is_power_of_two(), "pubkeys-per-shard must be a power of two");
    native_merkleize(pubkeys.iter().map(|pk| native_bytes_root(pk)).collect())
}

/// Native twin of [`sync_committee_root_from_subtrees`].
pub fn native_sync_committee_root_from_subtrees(
    subtree_roots: &[[u8; 32]],
    aggregate: &[u8; 48],
) -> [u8; 32] {
    assert!(subtree_roots.len().is_power_of_two(), "shard count must be a power of two");
    let pubkeys_vector_root = native_merkleize(subtree_roots.to_vec());
    let aggregate_root = native_bytes_root(aggregate);
    native_container_root(vec![pubkeys_vector_root, aggregate_root])
}
