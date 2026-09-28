//! `BridgeMultiHopProof` witness shapes — mirrors production GQL types
//! byte-for-byte, minus the DEX-only salt / anonymization plumbing.
//!
//! Field names and sizes match
//! `acki-nacki/helpers/proof_helper/src/gql_proof.rs`, so synthetic test
//! fixtures and live GQL payloads share a single shape. The bridge version
//! ported from `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_witness.rs`
//! keeps every native helper verbatim (block Merkle, L7 dense Merkle,
//! byte-flat Poseidon) and drops:
//!
//! - `HopWitness.salted_start_block_id` / `salted_end_block_id` (Fr) → replaced
//!   with `hop_start_block_id` / `hop_end_block_id` (`[u8; 32]`). The bridge
//!   has no anonymity requirement (spec §1.4, §9), so hops glue via clear
//!   block-ids.
//! - `MultiHopProofWitness.salt_commitment` (Fr) — no salt.
//! - `MultiHopProofWitness.bundle_index` (u32) — no per-snark position tag.
//!
//! ## Layout
//!
//! A bundle has up to `N_BUNDLE_MAX` `BridgeMultiHopProof` snarks, each
//! covering `H_HOPS_PER_PROOF = 1` hop (≤ 20 hops per bundle at the
//! prototype cap = `N_BUNDLE_MAX · H`). Per hop:
//!
//! - **Outer block-merkle**: `BLOCK_MERKLE_LEAF_COUNT = 16` SHA-256 leaves
//!   L0..L15 (depth-4 tree; only L7 is constrained at this level — it's the
//!   Poseidon root over the block's ref chain; production calls it
//!   `proof_block_refs_root`). L8..L15 are other block fields / zero padding
//!   that the hop circuit never opens; they still contribute to the outer
//!   SHA-256 walk against `block_id` via the depth-4 sibling path.
//! - **Inner ref-tree**: up to `MAX_PROOF_BLOCK_REFS` Poseidon leaves
//!   (`compute_referenced_block_leaf_hash(index, block_id)`), opened at
//!   `ref_index` to prove the parent block id of the hop chain.
//! - **`is_active`** padding flag (spec §4): inactive hops collapse to
//!   `hop_start_block_id == hop_end_block_id` (byte-equal), preserving the
//!   adjacent-hop continuity chain without needing a real ref opening.
//!
//! ## Poseidon hashing: byte-flat sponge everywhere
//!
//! Every Poseidon image in this kit is computed via `hash_bytes_flat` —
//! raw bytes are split into 31-byte chunks (top byte of each 32-byte Fr
//! buffer always zero ⇒ no silent mod-p reduction), each chunk becomes one
//! Fr, and the resulting `Vec<Fr>` is absorbed through Poseidon. This is
//! byte-for-byte identical to acki-nacki `node/libs/history-proof`
//! (`compute_referenced_block_leaf_hash`, `dense_combine`, …) and tvm-sdk
//! `PoseidonSponge::hash_bytes_flat`, so the kit's roots equal what GQL
//! would compute over the same block_ids.
//!
//! ## Constants — sourced from production
//!
//! - `BLOCK_MERKLE_LEAF_COUNT = 16`  ← `gql_proof.rs::BLOCK_MERKLE_LEAF_COUNT`
//! - `BLOCK_MERKLE_DEPTH = 4`        ← `gql_proof.rs::BLOCK_MERKLE_PROOF_DEPTH`
//! - `MAX_HISTORY_PROOF_LAYERS = 10` ← `gql_proof.rs:15`
//! - `HISTORY_PROOF_WINDOW_SIZE = 128` ← `history-proof/src/lib.rs`
//! - `REFERENCED_PARENT_BLOCK_TAG` / `REFERENCED_REF_BLOCK_TAG` ←
//!   `history-proof`
//! - `MAX_PROOF_BLOCK_REFS = 256` — protocol cap (spec §10.1). The L7 tree
//!   width is variable on the chain side (`leaves.len().next_power_of_two()`),
//!   so the circuit opens **up to** `MAX_PROOF_BLOCK_REFS_DEPTH = 8` and gates
//!   the fold with a per-hop `refs_tree_depth: u8` witness (spec §4).

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Constants — mirror production
// ---------------------------------------------------------------------------

/// SHA-256 leaves in the per-block outer merkle. **Frozen at 16** by the
/// production GQL layer (`gql_proof.rs::BLOCK_MERKLE_LEAF_COUNT`). L0 =
/// history-proofs Poseidon commit, L1..L6 = misc block fields, L7 =
/// `proof_block_refs_root` (Poseidon), L8 = `tracked_ext_out_messages_root`
/// (Poseidon), L9..L15 = zero padding (see acki-nacki
/// `node/src/types/ackinacki_block/mod.rs` `block_merkle_leaves`).
pub const BLOCK_MERKLE_LEAF_COUNT: usize = 16;

/// Depth of the per-block SHA-256 merkle (`log2(16) = 4`). Matches
/// `gql_proof.rs::BLOCK_MERKLE_PROOF_DEPTH`.
pub const BLOCK_MERKLE_DEPTH: usize = 4;

/// Maximum number of layers in the recursive history-proof chain
/// (`gql_proof.rs:15`).
pub const MAX_HISTORY_PROOF_LAYERS: usize = 10;

/// Window size used by `history-proof` (`HISTORY_PROOF_WINDOW_SIZE`). Each
/// layer covers `WINDOW_SIZE^layer` consecutive blocks.
pub const HISTORY_PROOF_WINDOW_SIZE: usize = 128;

/// Hops per BridgeMultiHopProof snark (spec §5).
///
/// **Set to 1 as a temporary stopgap** so the inner circuit's SHA-256 bill
/// (`prove_hop_block_merkle_sha256` = 4 SHA calls × 2 compressions × H
/// = 8 compressions) matches
/// `historical-layer-hashes-movement-checker-circuit` (8 compressions,
/// depth-4 L0 opening) and `BridgeEventFinalProof`'s L8 opening
/// (8 compressions out of its 15-compression total). Under this shape
/// the outer SHPLONK aggregator (`bridge-evm-aggregator`) fits under
/// EIP-170 (24,576 B) like the other three verifiers.
///
/// TODO: revisit `H_HOPS_PER_PROOF = 2` once we have either
///   (a) a Hermez PPoT K=22 SRS (currently only K=17..21 are available;
///       `crates/bridge-prover-libraries/params/kzg_bn254_22.srs` is
///       toxic-waste and refused by `AggregatorConfig`), so
///       `k_outer = 22` shrinks the outer verifier bytecode enough to
///       fit H=2 under EIP-170, **or**
///   (b) an inner circuit re-tune (K=18 with ~25 advice cols instead of
///       K=17 with 50) that halves the inner commitment count.
/// H=2 halves the number of multi-hop snarks per bundle (150 → 75 at
/// production `L_MAX = 300`), so it is desirable but not blocking.
/// See `crates/bridge-circuits/docs/SHA256_INVOCATIONS.md` §4 for the
/// H-vs-cost table.
pub const H_HOPS_PER_PROOF: usize = 1;

/// Maximum BridgeMultiHopProof snarks per bundle at the prototype cap
/// (spec §0: prototype `L_MAX = 20`, so `N_BUNDLE_MAX = ceil(20/1) = 20`).
/// Bump to 300 for production `L_MAX = 300`. Same-thread claims (`t = 0`,
/// `L = 0`) ship zero hop snarks — the bridge does not pad the bundle
/// length (spec §1.4). Value tracks `H_HOPS_PER_PROOF`; when that is
/// bumped back to 2, drop this to `ceil(L_MAX / 2)`.
pub const N_BUNDLE_MAX: usize = 20;

/// Protocol cap on L7 inner-Poseidon leaves (spec §10.1).
///
/// The chain-side L7 tree width is variable
/// (`leaves.len().next_power_of_two()`); this cap only bounds the fixed
/// padding of the in-circuit inner-path witness. Bump to widen the ceiling
/// — `MAX_PROOF_BLOCK_REFS_DEPTH` tracks it automatically.
pub const MAX_PROOF_BLOCK_REFS: usize = 256;

/// `ceil(log2(MAX_PROOF_BLOCK_REFS))` — tracks `MAX_PROOF_BLOCK_REFS`
/// automatically. The in-circuit fold walks this many levels and gates each
/// with `refs_tree_depth: u8` (spec §4).
pub const MAX_PROOF_BLOCK_REFS_DEPTH: usize =
    MAX_PROOF_BLOCK_REFS.next_power_of_two().ilog2() as usize;

/// Domain tag for the parent slot (index 0) in the ref-chain Poseidon tree.
/// Must equal `history-proof::REFERENCED_PARENT_BLOCK_TAG`.
pub const REFERENCED_PARENT_BLOCK_TAG: &[u8] = b"acki-nacki:referenced-block:parent:v1";

/// Domain tag for non-parent slots (index ≥ 1) in the ref-chain Poseidon tree.
/// Must equal `history-proof::REFERENCED_REF_BLOCK_TAG`.
pub const REFERENCED_REF_BLOCK_TAG: &[u8] = b"acki-nacki:referenced-block:ref:v1";

/// Assert that a hop's `ref_index` targets a cross-thread `refs` slot
/// (index ≥ 1). Slot 0 (`parent_block_id`) is same-thread by producer
/// construction (spec §2.3, §4); the bridge L7 walk never opens it.
///
/// Called by `test_helpers::synth_chain*` on every active hop so that
/// mis-populated witnesses fail loudly *before* the circuit's stricter
/// in-gate check fires (`ref_index != 0` in `multi_hop_proof.rs`).
pub fn assert_ref_index_is_cross_thread(ref_index: usize) {
    assert!(
        ref_index >= 1,
        "hop ref_index must be ≥ 1 (slot 0 is same-thread parent, excluded per spec §4); got {}",
        ref_index,
    );
    assert!(
        ref_index < MAX_PROOF_BLOCK_REFS,
        "hop ref_index {} exceeds MAX_PROOF_BLOCK_REFS {}",
        ref_index,
        MAX_PROOF_BLOCK_REFS,
    );
}

/// Native: `refs_tree_depth` for a variable-width L7 tree. Matches the
/// chain's `dense_merkle_tree` width convention
/// (`width = leaves.len().next_power_of_two()`, `depth = log2(width)`).
///
/// - `refs.len() == 0` ⇒ 0 (edge case, empty root).
/// - `refs.len() == 1` ⇒ 0 (leaf == root, no siblings).
/// - `refs.len() == 2` ⇒ 1.
/// - `refs.len() == 3..=4` ⇒ 2. Etc.
///
/// Result is always ≤ `MAX_PROOF_BLOCK_REFS_DEPTH` (guaranteed by the
/// `refs.len() ≤ MAX_PROOF_BLOCK_REFS` invariant enforced by native builders).
pub fn refs_tree_depth_native(refs: &[[u8; 32]]) -> u8 {
    let n = refs.len().max(1);
    n.next_power_of_two().ilog2() as u8
}

// ---------------------------------------------------------------------------
// Witness structs — field names mirror `gql_proof.rs` 1:1 where applicable
// ---------------------------------------------------------------------------

/// One block's GQL-shaped data — what the production verifier consumes per
/// block. Mirrors the inputs to `verify_gql_block_merkle` +
/// `verify_gql_proof_block_refs_l7`.
#[derive(Clone, Debug)]
pub struct BlockWitness {
    /// SHA-256 of the block: `block_merkle_root(block_merkle_tree_leaves)`.
    pub block_id: [u8; 32],

    /// The `BLOCK_MERKLE_LEAF_COUNT = 16` SHA-256 leaves L0..L15
    /// (`gql_proof.rs`'s `block_merkle_tree_leaves`). Leaf L7
    /// (`block_merkle_tree_leaves[7]`) equals
    /// `proof_block_refs_root(proof_block_refs)`; L8..L15 are opaque siblings
    /// (zero-padded on chain per §2.1 of the spec).
    pub block_merkle_tree_leaves: [[u8; 32]; BLOCK_MERKLE_LEAF_COUNT],

    /// The referenced-block-id list whose Poseidon dense-merkle root is L7.
    /// `proof_block_refs[0]` is the parent of `block_id` (tagged with
    /// `REFERENCED_PARENT_BLOCK_TAG`); `proof_block_refs[1..]` are
    /// referenced predecessors (tagged with `REFERENCED_REF_BLOCK_TAG`).
    ///
    /// Length is variable (≤ `MAX_PROOF_BLOCK_REFS`); the circuit pads to
    /// `MAX_PROOF_BLOCK_REFS` with an inactive padding leaf.
    pub proof_block_refs: Vec<[u8; 32]>,
}

/// One hop in the multi-hop chain — what the in-circuit
/// `BridgeMultiHopProof` opens per hop slot.
#[derive(Clone, Debug)]
pub struct HopWitness {
    /// Whether this hop slot is real or inactive padding (spec §4). When
    /// `false`, the circuit constrains `hop_start_block_id == hop_end_block_id`
    /// (byte-equal) and skips all openings below. Padded hops sit at the tail
    /// of a partially-full trailing snark; because bridge bundles are
    /// variable-length (spec §1.4) padded hops appear at most in one snark
    /// per bundle.
    pub is_active: bool,

    /// The hop's **start** block: `block.block_id` is what the hop's clear
    /// start endpoint publishes (Direction (a): start = current/newer block),
    /// and the hop's **end** block (the older ref) appears inside
    /// `block.proof_block_refs` (opened at `ref_index` against L7 via
    /// `proof_block_ref_inner_path`).
    pub block: BlockWitness,

    /// SHA-256 merkle opening for `block.block_merkle_tree_leaves[7]`
    /// against `block.block_id`. Always 4 siblings (depth = 4). For
    /// `leaf_index = 7`: `[L6, sha(L4,L5), sha(sha(L0,L1),sha(L2,L3)),
    /// sha(sha(sha(L8..L15 quads)))]` — i.e. the siblings of node index 7 at
    /// each level, cf. `gql_proof.rs::block_merkle_leaf_proof`.
    pub block_merkle_leaf_proof_l7: [[u8; 32]; BLOCK_MERKLE_DEPTH],

    /// Index of the *referenced parent* block within `block.proof_block_refs`.
    /// The bridge L7 walk requires `ref_index >= 1` (slot 0 is same-thread
    /// parent — spec §4).
    pub ref_index: usize,

    /// Real depth of the L7 dense-merkle tree for this hop, matching the
    /// chain's variable-width convention
    /// (`proof_block_refs.len().next_power_of_two().ilog2()`). Range
    /// `[0, MAX_PROOF_BLOCK_REFS_DEPTH]`. The circuit walks
    /// `MAX_PROOF_BLOCK_REFS_DEPTH` levels but gates each with a live-flag
    /// derived from this witness (spec §4).
    pub refs_tree_depth: u8,

    /// Dense-merkle siblings for opening `proof_block_refs[ref_index]`
    /// against L7, padded to `MAX_PROOF_BLOCK_REFS_DEPTH`. Only the first
    /// `refs_tree_depth` entries are real; the tail is zero-padding that the
    /// in-circuit fold ignores via the live-flag gate.
    pub proof_block_ref_inner_path: [[u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH],

    /// The hop's start endpoint as clear bytes — Direction (a): the *current*
    /// (newer) block whose L7 walk is closed by this hop, i.e.
    /// `block.block_id`. Published as `BridgeMultiHopProof.PI[0]` for the
    /// first hop in the snark.
    pub hop_start_block_id: [u8; 32],

    /// The hop's end endpoint as clear bytes — Direction (a): the *older*
    /// block extracted from `block.proof_block_refs[ref_index]`. Cross-hop
    /// continuity is enforced as a direct byte equality against the next
    /// hop's `hop_start_block_id` (which is that next hop's current block —
    /// so the ref becomes the following hop's L7 subject).
    pub hop_end_block_id: [u8; 32],
}

/// One BridgeMultiHopProof snark's worth of witness data — `H_HOPS_PER_PROOF`
/// hops, chained `hops[i].hop_end_block_id == hops[i+1].hop_start_block_id`.
///
/// Delta from DEX `MultiHopProofWitness`: no `salt_commitment`, no
/// `bundle_index`. The bridge glues snarks by clear block-ids and does not
/// need a per-snark position tag.
#[derive(Clone, Debug)]
pub struct MultiHopProofWitness {
    pub hops: [HopWitness; H_HOPS_PER_PROOF],
}

// ---------------------------------------------------------------------------
// Native SHA-256 depth-4 (16-leaf) merkle helpers — byte-identical to
// `gql_proof.rs`.
// ---------------------------------------------------------------------------

fn sha256_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(left);
    h.update(right);
    h.finalize().into()
}

fn sibling_index(index: usize) -> usize {
    if index % 2 == 0 {
        index + 1
    } else {
        index - 1
    }
}

/// SHA-256 subtree root over `leaves[start..start+width]`. `width` must be a
/// power of two ≤ `BLOCK_MERKLE_LEAF_COUNT`. Mirrors
/// `gql_proof.rs::block_merkle_subtree_root`.
fn block_merkle_subtree_root(
    leaves: &[[u8; 32]; BLOCK_MERKLE_LEAF_COUNT],
    start: usize,
    width: usize,
) -> [u8; 32] {
    match width {
        1 => leaves[start],
        2 => sha256_pair(&leaves[start], &leaves[start + 1]),
        4 | 8 | 16 => {
            let half = width / 2;
            let left = block_merkle_subtree_root(leaves, start, half);
            let right = block_merkle_subtree_root(leaves, start + half, half);
            sha256_pair(&left, &right)
        },
        _ => unreachable!("block Merkle subtree width is fixed to powers of two"),
    }
}

/// SHA-256 depth-4 merkle root over 16 leaves. Mirrors
/// `gql_proof.rs::block_merkle_root` exactly.
pub fn block_merkle_root(leaves: &[[u8; 32]; BLOCK_MERKLE_LEAF_COUNT]) -> [u8; 32] {
    block_merkle_subtree_root(leaves, 0, BLOCK_MERKLE_LEAF_COUNT)
}

/// 4-sibling path proving `leaves[leaf_index]` against `block_merkle_root`.
/// Mirrors `gql_proof.rs::block_merkle_leaf_proof` exactly.
pub fn block_merkle_leaf_proof(
    leaves: &[[u8; 32]; BLOCK_MERKLE_LEAF_COUNT],
    leaf_index: usize,
) -> [[u8; 32]; BLOCK_MERKLE_DEPTH] {
    assert!(
        leaf_index < BLOCK_MERKLE_LEAF_COUNT,
        "leaf_index {leaf_index} out of range"
    );
    let mut proof = [[0u8; 32]; BLOCK_MERKLE_DEPTH];
    proof[0] = leaves[sibling_index(leaf_index)];
    proof[1] = block_merkle_subtree_root(leaves, sibling_index(leaf_index / 2) * 2, 2);
    proof[2] = block_merkle_subtree_root(leaves, sibling_index(leaf_index / 4) * 4, 4);
    proof[3] = block_merkle_subtree_root(leaves, sibling_index(leaf_index / 8) * 8, 8);
    proof
}

/// Verify a 4-sibling SHA-256 path against `root`. Mirrors
/// `gql_proof.rs::verify_block_merkle_leaf_proof`.
pub fn verify_block_merkle_leaf_proof(
    root: &[u8; 32],
    leaf: &[u8; 32],
    leaf_index: usize,
    proof: &[[u8; 32]; BLOCK_MERKLE_DEPTH],
) -> bool {
    if leaf_index >= BLOCK_MERKLE_LEAF_COUNT {
        return false;
    }
    let mut cur = *leaf;
    let mut idx = leaf_index;
    for sibling in proof {
        cur = if idx % 2 == 0 {
            sha256_pair(&cur, sibling)
        } else {
            sha256_pair(sibling, &cur)
        };
        idx /= 2;
    }
    &cur == root
}

// ---------------------------------------------------------------------------
// L7 inner ref-chain helpers — byte-flat Poseidon sponge
// ---------------------------------------------------------------------------
//
// Byte-for-byte mirrors of acki-nacki `node/libs/history-proof`
// (`compute_referenced_block_leaf_hash`, `dense_combine`,
// `compute_referenced_blocks_root`) and tvm-sdk
// `PoseidonSponge::hash_bytes_flat`. The raw byte concatenation is chunked
// at 31-byte boundaries with top byte zero ⇒ every Fr input is strictly
// less than the BN254 Fr modulus, no silent mod-p reduction. Sponge params
// (`T=3, RATE=2, R_F=8, R_P=57`) come from `gosh_dense_balanced_tree`.

use gosh_dense_balanced_tree::{bytes_to_fr, fr_to_bytes, poseidon_hash_native};

/// Native: Poseidon sponge over a raw byte stream, identical to
/// `PoseidonSponge::hash_bytes_flat` in tvm-sdk.
///
/// Chunks `bytes` into 31-byte windows, zero-pads the last to 32, interprets
/// each chunk as a little-endian `Fr`, and absorbs the resulting `Vec<Fr>`
/// through `poseidon_hash_native`. The top byte of each 32-byte Fr buffer is
/// always zero (chunk size = 31), so each absorbed value is guaranteed to be
/// strictly less than the BN254 Fr modulus.
pub fn poseidon_bytes_flat_native(bytes: &[u8]) -> [u8; 32] {
    const CHUNK: usize = 31;
    let mut inputs: Vec<Fr> = Vec::with_capacity((bytes.len() + CHUNK - 1) / CHUNK);
    for window in bytes.chunks(CHUNK) {
        let mut buf = [0u8; 32];
        buf[..window.len()].copy_from_slice(window);
        inputs.push(bytes_to_fr(&buf));
    }
    if inputs.is_empty() {
        // Match production: empty input still goes through one chunk of zeros.
        inputs.push(bytes_to_fr(&[0u8; 32]));
    }
    fr_to_bytes(poseidon_hash_native(&inputs))
}

/// Byte-flat ref-leaf chunk0 constant: LE-pack of
/// `REFERENCED_PARENT_BLOCK_TAG[0..31]`.
///
/// The byte-flat encoding of `tag(37B) || parent_id(32B)` (69 bytes total)
/// splits into three 31-byte chunks: `chunk0 = data[0..31]` is the tag's
/// first 31 bytes — entirely constant, so callers load it once via
/// `ctx.load_constant`.
pub fn ref_leaf_parent_tag_chunk0_fr() -> Fr {
    let bytes = REFERENCED_PARENT_BLOCK_TAG;
    let mut buf = [0u8; 32];
    buf[..31].copy_from_slice(&bytes[..31]);
    bytes_to_fr(&buf)
}

/// Byte-flat ref-leaf chunk1 constant-tail: LE-pack of
/// `REFERENCED_PARENT_BLOCK_TAG[31..37]`.
///
/// `chunk1 = data[31..62]` covers the last 6 tag bytes followed by
/// `parent_id[0..25]`. The constant-tail (these 6 bytes packed at LE
/// positions 0..6 of the chunk) is loaded once; the witness contribution
/// (`parent_id[0..25]` packed at LE positions 6..31) is added in-circuit
/// via `inner_product(parent_id[0..25], [256^6, ..., 256^30])`.
pub fn ref_leaf_parent_tag_chunk1_lo_fr() -> Fr {
    let bytes = REFERENCED_PARENT_BLOCK_TAG;
    let mut buf = [0u8; 32];
    buf[..6].copy_from_slice(&bytes[31..37]);
    bytes_to_fr(&buf)
}

/// Byte-flat ref-leaf chunk0 constant for the **ref-tag** layout (ref_index ≥
/// 1): LE-pack of `REFERENCED_REF_BLOCK_TAG[0..31]`.
///
/// Used when the ref-leaf is opened at a non-parent slot. The byte-flat
/// encoding of `tag(34 B) ‖ id(32 B)` (66 bytes total) splits into three
/// 31-byte chunks: `chunk0 = data[0..31]` is the tag's first 31 bytes —
/// entirely constant.
pub fn ref_leaf_ref_tag_chunk0_fr() -> Fr {
    let bytes = REFERENCED_REF_BLOCK_TAG;
    let mut buf = [0u8; 32];
    buf[..31].copy_from_slice(&bytes[..31]);
    bytes_to_fr(&buf)
}

/// Byte-flat ref-leaf chunk1 constant-tail for the **ref-tag** layout:
/// LE-pack of `REFERENCED_REF_BLOCK_TAG[31..34]`.
///
/// `chunk1 = data[31..62]` covers the last 3 tag bytes followed by
/// `id[0..28]`. The constant-tail (these 3 bytes packed at LE positions
/// 0..3 of the chunk) is loaded once; the witness contribution (`id[0..28]`
/// packed at LE positions 3..31) is added in-circuit via
/// `inner_product(id[0..28], [256^3, ..., 256^30])`.
pub fn ref_leaf_ref_tag_chunk1_lo_fr() -> Fr {
    let bytes = REFERENCED_REF_BLOCK_TAG;
    let mut buf = [0u8; 32];
    buf[..3].copy_from_slice(&bytes[31..34]);
    bytes_to_fr(&buf)
}

/// Native: per-ref leaf hash matching
/// `history-proof::compute_referenced_block_leaf_hash` byte-for-byte. Index 0
/// uses the parent tag, ≥1 uses the ref tag, and the entire `tag ‖ block_id`
/// byte stream is fed through `poseidon_bytes_flat_native`.
pub fn ref_leaf_hash_native(index: usize, block_id: &[u8; 32]) -> [u8; 32] {
    let tag_bytes: &[u8] = if index == 0 {
        REFERENCED_PARENT_BLOCK_TAG
    } else {
        REFERENCED_REF_BLOCK_TAG
    };
    let mut concat = Vec::with_capacity(tag_bytes.len() + 32);
    concat.extend_from_slice(tag_bytes);
    concat.extend_from_slice(block_id);
    poseidon_bytes_flat_native(&concat)
}

/// Native: pairwise combiner matching production's `dense_combine`
/// (`hash_bytes_flat(left ‖ right)`).
pub fn ref_inner_combine_native(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut concat = [0u8; 64];
    concat[..32].copy_from_slice(left);
    concat[32..].copy_from_slice(right);
    poseidon_bytes_flat_native(&concat)
}

/// Native: L7 root computation matching production
/// `compute_referenced_blocks_root` byte-for-byte. Uses the chain's
/// variable-width convention (`width = leaves.len().next_power_of_two()`)
/// padded with the inactive padding leaf `fr_to_bytes(Fr::from(0))`, so the
/// result equals the chain's dense-merkle root exactly.
///
/// Edge cases:
/// - `refs.len() == 0` ⇒ single zero leaf treated as the root.
/// - `refs.len() == 1` ⇒ the single ref-leaf-hash IS the root (depth 0).
pub fn proof_block_refs_root_native(proof_block_refs: &[[u8; 32]]) -> [u8; 32] {
    assert!(
        proof_block_refs.len() <= MAX_PROOF_BLOCK_REFS,
        "proof_block_refs len {} exceeds MAX_PROOF_BLOCK_REFS {}",
        proof_block_refs.len(),
        MAX_PROOF_BLOCK_REFS
    );

    let width = proof_block_refs.len().max(1).next_power_of_two();
    let mut layer: Vec<[u8; 32]> = (0..width)
        .map(|i| {
            if i < proof_block_refs.len() {
                ref_leaf_hash_native(i, &proof_block_refs[i])
            } else {
                fr_to_bytes(Fr::from(0u64))
            }
        })
        .collect();

    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(ref_inner_combine_native(&pair[0], &pair[1]));
        }
        layer = next;
    }
    layer[0]
}

/// Native: opening of `proof_block_refs[ref_index]` against the L7 root, with
/// the sibling path padded to `MAX_PROOF_BLOCK_REFS_DEPTH`. Only the first
/// `refs_tree_depth_native(proof_block_refs)` siblings are real; the tail is
/// zero-padding that the in-circuit gated fold ignores.
///
/// Returns `(padded_siblings, real_depth)` so callers can populate both the
/// `proof_block_ref_inner_path` and `refs_tree_depth` witness fields in one
/// call.
pub fn proof_block_ref_inner_path_native(
    proof_block_refs: &[[u8; 32]],
    ref_index: usize,
) -> ([[u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH], u8) {
    assert!(
        !proof_block_refs.is_empty(),
        "proof_block_refs must be non-empty"
    );
    assert!(
        ref_index < proof_block_refs.len(),
        "ref_index {ref_index} ≥ proof_block_refs.len() {}",
        proof_block_refs.len()
    );
    assert!(
        proof_block_refs.len() <= MAX_PROOF_BLOCK_REFS,
        "proof_block_refs len {} exceeds MAX_PROOF_BLOCK_REFS {}",
        proof_block_refs.len(),
        MAX_PROOF_BLOCK_REFS
    );

    let depth = refs_tree_depth_native(proof_block_refs) as usize;
    let width = 1usize << depth;

    let mut layer: Vec<[u8; 32]> = (0..width)
        .map(|i| {
            if i < proof_block_refs.len() {
                ref_leaf_hash_native(i, &proof_block_refs[i])
            } else {
                fr_to_bytes(Fr::from(0u64))
            }
        })
        .collect();

    let mut idx = ref_index;
    let mut siblings = [[0u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH];
    for d in 0..depth {
        let sib_idx = idx ^ 1;
        siblings[d] = layer[sib_idx];
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(ref_inner_combine_native(&pair[0], &pair[1]));
        }
        layer = next;
        idx /= 2;
    }
    // Levels `[depth, MAX_PROOF_BLOCK_REFS_DEPTH)` stay zeroed — ignored
    // in-circuit via the live-flag gate.
    (siblings, depth as u8)
}

/// Verify a `proof_block_ref_inner_path_native` opening. `refs_tree_depth`
/// is the real depth; padding levels beyond it are ignored.
pub fn verify_proof_block_ref_inner_path(
    root: &[u8; 32],
    leaf: &[u8; 32],
    ref_index: usize,
    siblings: &[[u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH],
    refs_tree_depth: u8,
) -> bool {
    let depth = refs_tree_depth as usize;
    if depth > MAX_PROOF_BLOCK_REFS_DEPTH {
        return false;
    }
    let mut cur = *leaf;
    let mut idx = ref_index;
    for sib in siblings.iter().take(depth) {
        cur = if idx % 2 == 0 {
            ref_inner_combine_native(&cur, sib)
        } else {
            ref_inner_combine_native(sib, &cur)
        };
        idx /= 2;
    }
    &cur == root
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_gql_proof_layout() {
        assert_eq!(BLOCK_MERKLE_LEAF_COUNT, 16);
        assert_eq!(BLOCK_MERKLE_DEPTH, 4);
        assert_eq!(1 << BLOCK_MERKLE_DEPTH, BLOCK_MERKLE_LEAF_COUNT);
        assert_eq!(MAX_HISTORY_PROOF_LAYERS, 10);
        assert_eq!(1 << MAX_PROOF_BLOCK_REFS_DEPTH, MAX_PROOF_BLOCK_REFS);
    }

    #[test]
    fn n_bundle_max_matches_prototype_l_max() {
        // Prototype L_MAX = 20, H = 1 ⇒ ceil(20/1) = 20.
        assert_eq!(N_BUNDLE_MAX, 20);
        assert_eq!(H_HOPS_PER_PROOF, 1);
    }

    #[test]
    fn block_merkle_root_and_proof_roundtrip() {
        let leaves: [[u8; 32]; BLOCK_MERKLE_LEAF_COUNT] =
            std::array::from_fn(|i| [i as u8 + 1; 32]);
        let root = block_merkle_root(&leaves);
        for i in 0..BLOCK_MERKLE_LEAF_COUNT {
            let proof = block_merkle_leaf_proof(&leaves, i);
            assert!(
                verify_block_merkle_leaf_proof(&root, &leaves[i], i, &proof),
                "leaf {i} proof should verify"
            );
        }
    }

    #[test]
    fn block_merkle_proof_rejects_tampering() {
        let leaves: [[u8; 32]; BLOCK_MERKLE_LEAF_COUNT] =
            std::array::from_fn(|i| [i as u8 + 10; 32]);
        let root = block_merkle_root(&leaves);
        let mut proof = block_merkle_leaf_proof(&leaves, 3);
        proof[1] = [0xFFu8; 32];
        assert!(!verify_block_merkle_leaf_proof(
            &root, &leaves[3], 3, &proof
        ));
    }

    #[test]
    fn proof_block_refs_root_and_inner_path_roundtrip() {
        // Exercise widths across the spectrum:
        //   n = 1 ⇒ depth 0 (leaf == root)
        //   n = 2 ⇒ depth 1
        //   n = 5 ⇒ width 8, depth 3 (padded)
        //   n = 16 ⇒ width 16, depth 4
        //   n = 40 ⇒ width 64, depth 6 (padded)
        //
        // The full n = 200 (depth 8, MAX_PROOF_BLOCK_REFS_DEPTH) sweep is
        // covered by the `#[ignore]`d `proof_block_refs_max_depth_roundtrip`
        // stress case below — running it inline balloons the fast MockProver
        // step of `bridge-circuits.yaml` from milliseconds to minutes because
        // the test re-derives the tree once per ref (~O(n^2) Poseidon
        // calls).
        for n in [1usize, 2, 5, 16, 40] {
            let refs: Vec<[u8; 32]> = (0..n)
                .map(|i| {
                    let mut r = [0u8; 32];
                    r[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                    r
                })
                .collect();
            let root = proof_block_refs_root_native(&refs);
            let depth = refs_tree_depth_native(&refs);
            assert!(
                (depth as usize) <= MAX_PROOF_BLOCK_REFS_DEPTH,
                "depth {depth} must be ≤ {MAX_PROOF_BLOCK_REFS_DEPTH}"
            );
            for (i, r) in refs.iter().enumerate() {
                let leaf = ref_leaf_hash_native(i, r);
                let (siblings, d) = proof_block_ref_inner_path_native(&refs, i);
                assert_eq!(d, depth, "inner-path depth must match root depth");
                assert!(
                    verify_proof_block_ref_inner_path(&root, &leaf, i, &siblings, d),
                    "ref {i} inner-path should verify (n = {n})"
                );
            }
        }
    }

    #[test]
    fn refs_tree_depth_native_matches_next_power_of_two() {
        assert_eq!(refs_tree_depth_native(&[]), 0);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 1]), 0);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 2]), 1);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 3]), 2);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 4]), 2);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 5]), 3);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 8]), 3);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 9]), 4);
        assert_eq!(refs_tree_depth_native(&[[0u8; 32]; 256]), 8);
    }

    #[test]
    fn parent_and_ref_tags_distinct() {
        let block_id = [0xAAu8; 32];
        let parent_leaf = ref_leaf_hash_native(0, &block_id);
        let ref_leaf = ref_leaf_hash_native(1, &block_id);
        assert_ne!(
            parent_leaf, ref_leaf,
            "parent-tag and ref-tag must produce distinct leaf hashes"
        );
    }

    /// `poseidon_bytes_flat_native` on a 32-byte input chunks into 31 + 1 byte
    /// pieces — verify that the function output matches running the chunking
    /// by hand.
    #[test]
    fn poseidon_bytes_flat_chunks_match_manual() {
        let input = [0xCCu8; 32];
        let manual = {
            let mut c0 = [0u8; 32];
            c0[..31].copy_from_slice(&input[..31]);
            let mut c1 = [0u8; 32];
            c1[0] = input[31];
            fr_to_bytes(poseidon_hash_native(&[bytes_to_fr(&c0), bytes_to_fr(&c1)]))
        };
        let via_fn = poseidon_bytes_flat_native(&input);
        assert_eq!(manual, via_fn);
    }

    /// Stress case at max protocol depth (`MAX_PROOF_BLOCK_REFS_DEPTH = 8`).
    /// Gated `#[ignore]` because it does ~O(n^2) native Poseidon
    /// evaluations and dominates the fast MockProver step wall time; run
    /// explicitly with `cargo test -- --ignored proof_block_refs_max_depth`
    /// when touching the L7 helpers.
    #[test]
    #[ignore]
    fn proof_block_refs_max_depth_roundtrip() {
        let n = 200usize;
        let refs: Vec<[u8; 32]> = (0..n)
            .map(|i| {
                let mut r = [0u8; 32];
                r[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                r
            })
            .collect();
        let root = proof_block_refs_root_native(&refs);
        let depth = refs_tree_depth_native(&refs);
        assert_eq!(depth as usize, MAX_PROOF_BLOCK_REFS_DEPTH);
        for (i, r) in refs.iter().enumerate() {
            let leaf = ref_leaf_hash_native(i, r);
            let (siblings, d) = proof_block_ref_inner_path_native(&refs, i);
            assert_eq!(d, depth);
            assert!(verify_proof_block_ref_inner_path(
                &root, &leaf, i, &siblings, d
            ));
        }
    }

    #[test]
    #[should_panic(expected = "hop ref_index must be ≥ 1")]
    fn assert_ref_index_rejects_parent_slot() {
        assert_ref_index_is_cross_thread(0);
    }

    #[test]
    #[should_panic(expected = "exceeds MAX_PROOF_BLOCK_REFS")]
    fn assert_ref_index_rejects_overflow() {
        assert_ref_index_is_cross_thread(MAX_PROOF_BLOCK_REFS);
    }
}
