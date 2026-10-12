//! M2 — in-circuit SSZ merkleization primitives over `gosh-sha256-chip`.
//!
//! A **node** is a 32-byte hash represented as `Vec<AssignedValue<F>>` (len 32,
//! each a byte in `[0,255]`). All hashing is real in-circuit SHA-256
//! (`Sha256Chip::digest_bytes`), so every root produced here is soundly bound to
//! its preimage — no off-circuit trust.
//!
//! SSZ merkleization rules implemented (consensus-specs `ssz/merkle-proofs`):
//! - `sha256_pair(l, r)` = `SHA256(l ‖ r)` (64→32).
//! - `merkleize(leaves)` = binary tree over `2^k` leaves (caller pads leaf
//!   **count** to a power of two with zero nodes; container/vector padding).
//! - `verify_merkle_branch(leaf, branch, gindex, root)` — the SSZ single-leaf
//!   proof. `gindex` is a **compile-time constant** (from the M0 fork table), so
//!   the left/right ordering at each level is fixed at build time (sound: the
//!   circuit is specialized to one generalized index).
//!
//! Every in-circuit function has a `native_*` twin (plain `sha2`) used by tests
//! to assert byte-for-byte equality via MockProver.

use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::GateInstructions;
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, Context};
use sha2::{Digest, Sha256};

/// A 32-byte SSZ node as assigned bytes.
pub type Node<F> = Vec<AssignedValue<F>>;

/// Load a 32-byte value as byte witnesses.
pub fn load_node<F: BigPrimeField>(ctx: &mut Context<F>, bytes: &[u8; 32]) -> Node<F> {
    bytes.iter().map(|&b| ctx.load_witness(F::from(b as u64))).collect()
}

/// Load an arbitrary byte slice as byte witnesses.
pub fn load_bytes<F: BigPrimeField>(ctx: &mut Context<F>, bytes: &[u8]) -> Vec<AssignedValue<F>> {
    bytes.iter().map(|&b| ctx.load_witness(F::from(b as u64))).collect()
}

/// A constant all-zero 32-byte node.
pub fn zero_node<F: BigPrimeField>(ctx: &mut Context<F>) -> Node<F> {
    (0..32).map(|_| ctx.load_constant(F::ZERO)).collect()
}

/// `SHA256(left ‖ right)` over two 32-byte nodes.
pub fn sha256_pair<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    left: &[AssignedValue<F>],
    right: &[AssignedValue<F>],
) -> Node<F> {
    debug_assert_eq!(left.len(), 32);
    debug_assert_eq!(right.len(), 32);
    let mut input = Vec::with_capacity(64);
    input.extend_from_slice(left);
    input.extend_from_slice(right);
    chip.digest_bytes(ctx, &input)
}

/// Merkleize `leaves` (length **must** be a power of two) into a single root.
pub fn merkleize<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    mut layer: Vec<Node<F>>,
) -> Node<F> {
    assert!(!layer.is_empty(), "merkleize needs >= 1 leaf");
    assert!(layer.len().is_power_of_two(), "leaf count must be a power of two");
    while layer.len() > 1 {
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(sha256_pair(chip, ctx, &pair[0], &pair[1]));
        }
        layer = next;
    }
    layer.pop().unwrap()
}

/// `hash_tree_root` of a container: pad the `fields` (field roots) to the next
/// power-of-two count with zero nodes, then merkleize.
pub fn container_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    mut fields: Vec<Node<F>>,
) -> Node<F> {
    let target = fields.len().next_power_of_two();
    while fields.len() < target {
        fields.push(zero_node(ctx));
    }
    merkleize(chip, ctx, fields)
}

/// `hash_tree_root` of a fixed byte vector (`BytesN`): chunk into 32-byte pieces
/// (last zero-padded), pad chunk count to a power of two, merkleize. A single
/// chunk (`N <= 32`) is its own root (no hashing), per SSZ.
pub fn bytes_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    bytes: &[AssignedValue<F>],
) -> Node<F> {
    let mut chunks: Vec<Node<F>> = bytes
        .chunks(32)
        .map(|c| {
            let mut n = c.to_vec();
            while n.len() < 32 {
                n.push(ctx.load_constant(F::ZERO));
            }
            n
        })
        .collect();
    if chunks.len() == 1 {
        return chunks.pop().unwrap();
    }
    let target = chunks.len().next_power_of_two();
    while chunks.len() < target {
        chunks.push(zero_node(ctx));
    }
    merkleize(chip, ctx, chunks)
}

/// `hash_tree_root` of a `uint64` (SSZ basic type): 8 little-endian bytes,
/// right-padded to 32. `le_bytes` are the 8 value bytes (assigned).
pub fn uint64_root<F: BigPrimeField>(
    ctx: &mut Context<F>,
    le_bytes: &[AssignedValue<F>],
) -> Node<F> {
    assert_eq!(le_bytes.len(), 8, "uint64 needs 8 LE bytes");
    let mut node: Node<F> = le_bytes.to_vec();
    for _ in 0..24 {
        node.push(ctx.load_constant(F::ZERO));
    }
    node
}

/// Verify an SSZ single-leaf Merkle proof and constrain the recomputed root to
/// equal `root`. `gindex` is a compile-time constant (M0 fork table).
pub fn verify_merkle_branch<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    leaf: &Node<F>,
    branch: &[Node<F>],
    gindex: u64,
    root: &Node<F>,
) {
    let depth = merkle_depth(gindex);
    assert_eq!(branch.len(), depth, "branch length must equal floor(log2(gindex))");
    let mut node = leaf.clone();
    for (i, sibling) in branch.iter().enumerate() {
        node = if (gindex >> i) & 1 == 0 {
            sha256_pair(chip, ctx, &node, sibling)
        } else {
            sha256_pair(chip, ctx, sibling, &node)
        };
    }
    for j in 0..32 {
        ctx.constrain_equal(&node[j], &root[j]);
    }
}

/// Verify an SSZ single-leaf proof whose generalized index is one of two
/// compile-time constants, picked by the boolean cell `use_b` (`0` → `gindex_a`,
/// `1` → `gindex_b`), and constrain the recomputed root to equal `root`.
///
/// `branch` holds `max(depth_a, depth_b)` nodes, sibling of the leaf first. For
/// the shallower index only its first `depth` nodes are used; the rest are
/// ignored. That is the reverse of the spec's normalized layout, which pads at
/// the front: convert with [`branch_for_either`].
#[allow(clippy::too_many_arguments)]
pub fn verify_merkle_branch_either<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    gate: &impl GateInstructions<F>,
    ctx: &mut Context<F>,
    leaf: &Node<F>,
    branch: &[Node<F>],
    gindex_a: u64,
    gindex_b: u64,
    use_b: AssignedValue<F>,
    root: &Node<F>,
) {
    let (depth_a, depth_b) = (merkle_depth(gindex_a), merkle_depth(gindex_b));
    assert_eq!(branch.len(), depth_a.max(depth_b), "branch length must equal the larger depth");
    gate.assert_bit(ctx, use_b);
    let use_a = gate.not(ctx, use_b);
    let mut node = leaf.clone();
    for (i, sibling) in branch.iter().enumerate() {
        let level_bit = |gindex: u64, depth: usize| i < depth && (gindex >> i) & 1 == 1;
        let level_active = |depth: usize| i < depth;
        let sibling_left = select_const(
            ctx,
            use_b,
            use_a,
            level_bit(gindex_b, depth_b),
            level_bit(gindex_a, depth_a),
        );
        let active = select_const(
            ctx,
            use_b,
            use_a,
            level_active(depth_b),
            level_active(depth_a),
        );
        let mut left = Vec::with_capacity(32);
        let mut right = Vec::with_capacity(32);
        for j in 0..32 {
            left.push(gate.select(ctx, sibling[j], node[j], sibling_left));
            right.push(gate.select(ctx, node[j], sibling[j], sibling_left));
        }
        let hashed = sha256_pair(chip, ctx, &left, &right);
        node = (0..32).map(|j| gate.select(ctx, hashed[j], node[j], active)).collect();
    }
    for j in 0..32 {
        ctx.constrain_equal(&node[j], &root[j]);
    }
}

/// `use_b ? b : a` for two constant booleans, as a cell (`use_a = 1 - use_b`).
fn select_const<F: BigPrimeField>(
    ctx: &mut Context<F>,
    use_b: AssignedValue<F>,
    use_a: AssignedValue<F>,
    b: bool,
    a: bool,
) -> AssignedValue<F> {
    match (b, a) {
        (true, true) => ctx.load_constant(F::ONE),
        (false, false) => ctx.load_constant(F::ZERO),
        (true, false) => use_b,
        (false, true) => use_a,
    }
}

/// Native twin of [`verify_merkle_branch_either`]: the root `branch` folds `leaf`
/// to under the selected index.
pub fn native_merkle_branch_root_either(
    leaf: &[u8; 32],
    branch: &[[u8; 32]],
    gindex_a: u64,
    gindex_b: u64,
    use_b: bool,
) -> [u8; 32] {
    let gindex = if use_b { gindex_b } else { gindex_a };
    assert_eq!(branch.len(), merkle_depth(gindex_a).max(merkle_depth(gindex_b)));
    native_merkle_branch_root(leaf, &branch[..merkle_depth(gindex)], gindex)
}

/// Turn a spec branch for `gindex` into the layout [`verify_merkle_branch_either`]
/// takes: `len` nodes, the real siblings first, zero padding after.
///
/// Accepts both the plain branch (`floor(log2(gindex))` nodes) and the
/// spec's normalized one (`normalize_merkle_branch`: zero nodes in front, as
/// Gloas-era beacon APIs serve pre-Gloas data). Rejects non-zero padding, like
/// `is_valid_normalized_merkle_branch`.
pub fn branch_for_either(
    branch: &[[u8; 32]],
    gindex: u64,
    len: usize,
) -> Result<Vec<[u8; 32]>, String> {
    let depth = merkle_depth(gindex);
    if branch.len() < depth || branch.len() > len.max(depth) {
        return Err(format!(
            "branch for gindex {gindex} has {} nodes; expected {depth}..={len}",
            branch.len()
        ));
    }
    let extra = branch.len() - depth;
    if branch[..extra].iter().any(|n| *n != [0u8; 32]) {
        return Err(format!("normalized branch for gindex {gindex} has non-zero padding"));
    }
    let mut out = branch[extra..].to_vec();
    out.resize(len, [0u8; 32]);
    Ok(out)
}

/// Branch depth for a generalized index = `floor(log2(gindex))`.
pub fn merkle_depth(gindex: u64) -> usize {
    assert!(gindex >= 1, "gindex must be >= 1");
    (63 - gindex.leading_zeros()) as usize
}

// ---------------------------------------------------------------------------
// Native (off-circuit) references — used by tests to check the circuit output.
// ---------------------------------------------------------------------------

pub fn native_sha256_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(left);
    h.update(right);
    h.finalize().into()
}

pub fn native_merkleize(mut layer: Vec<[u8; 32]>) -> [u8; 32] {
    assert!(layer.len().is_power_of_two());
    while layer.len() > 1 {
        layer = layer
            .chunks(2)
            .map(|p| native_sha256_pair(&p[0], &p[1]))
            .collect();
    }
    layer[0]
}

pub fn native_container_root(mut fields: Vec<[u8; 32]>) -> [u8; 32] {
    let target = fields.len().next_power_of_two();
    fields.resize(target, [0u8; 32]);
    native_merkleize(fields)
}

/// SSZ Merkle proof of field `index` in a container of `fields` (padded to a
/// power of two), leaf sibling first. Its generalized index is
/// `fields.len().next_power_of_two() + index`.
pub fn native_container_proof(mut fields: Vec<[u8; 32]>, index: usize) -> Vec<[u8; 32]> {
    assert!(index < fields.len());
    let target = fields.len().next_power_of_two();
    fields.resize(target, [0u8; 32]);
    let mut proof = Vec::new();
    let mut idx = index;
    while fields.len() > 1 {
        proof.push(fields[idx ^ 1]);
        fields = fields.chunks(2).map(|p| native_sha256_pair(&p[0], &p[1])).collect();
        idx /= 2;
    }
    proof
}

pub fn native_bytes_root(bytes: &[u8]) -> [u8; 32] {
    let mut chunks: Vec<[u8; 32]> = bytes
        .chunks(32)
        .map(|c| {
            let mut n = [0u8; 32];
            n[..c.len()].copy_from_slice(c);
            n
        })
        .collect();
    if chunks.len() == 1 {
        return chunks[0];
    }
    let target = chunks.len().next_power_of_two();
    chunks.resize(target, [0u8; 32]);
    native_merkleize(chunks)
}

pub fn native_uint64_root(value: u64) -> [u8; 32] {
    let mut n = [0u8; 32];
    n[..8].copy_from_slice(&value.to_le_bytes());
    n
}

pub fn native_merkle_branch_root(leaf: &[u8; 32], branch: &[[u8; 32]], gindex: u64) -> [u8; 32] {
    assert_eq!(branch.len(), merkle_depth(gindex));
    let mut node = *leaf;
    for (i, sib) in branch.iter().enumerate() {
        node = if (gindex >> i) & 1 == 0 {
            native_sha256_pair(&node, sib)
        } else {
            native_sha256_pair(sib, &node)
        };
    }
    node
}
