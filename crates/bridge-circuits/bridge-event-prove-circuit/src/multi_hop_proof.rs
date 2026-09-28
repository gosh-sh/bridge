//! `BridgeMultiHopProof` — cross-thread L7 hop-chain snark.
//!
//! Proves `H_HOPS_PER_PROOF = 1` hop in one snark. Each *active* hop opens
//! `proof_block_refs[ref_index]` against the block's L7 (Poseidon dense-merkle
//! root) and L7 against `block.block_id` (SHA-256 depth-4 merkle at leaf 7).
//! Adjacent hops are chained by **clear-byte** equality of block-ids; inactive
//! padding hops satisfy `hop_start_block_id == hop_end_block_id` (byte-wise)
//! so the padding tail of a partially-full snark propagates the terminal
//! block-id.
//!
//! # Walk direction (Direction (a), spec §3, cross-thread reachability)
//!
//! Bridge Circuit 4 commits to Direction (a): the event `X` sits on a
//! non-default thread `t` and is *newer*; the anchor `Y` sits on the default
//! thread and is *older*. The walk crawls **newest → oldest** via
//! `proof_block_refs` (chain `refs` only ever point at strictly older blocks).
//! Per hop, `hop_start_block_id` is the *current block being opened* (newer)
//! and `hop_end_block_id` is the *ref extracted from that block* (older).
//!
//! So for the whole hop-chain snark:
//!
//! * `first_start = hops[0].hop_start_block_id` = `X` (event block, newer)
//! * `last_end    = hops[H-1].hop_end_block_id` = `Y` (anchor block, older)
//!
//! Ported from `dexdo-halo2-kit/dex-halo2-circuit/src/multi_hop_proof.rs`,
//! minus the DEX-only salt / anonymity plumbing (spec §1.4, §9):
//! - No `sk_u`, no `salt`, no `salt_commitment`, no salted-endpoint Poseidon
//!   gadget.
//! - No `bundle_index` witness, no position-tag mixing.
//! - Hops glue via clear `[u8; 32]` block-ids instead of position-tagged
//!   `Poseidon(salt, block_id, position)` outputs.
//!
//! # Public instances (`MULTI_HOP_PUBLIC_LEN = 2`)
//!
//! | idx | name                  | derivation                                    |
//! |-----|-----------------------|-----------------------------------------------|
//! | 0   | `hop_start_block_id`  | LE-pack of `hops[0].hop_start_block_id` (`X`) |
//! | 1   | `hop_end_block_id`    | LE-pack of `hops[H-1].hop_end_block_id` (`Y`) |
//!
//! Adjacent-hop continuity (`hops[i].hop_end == hops[i+1].hop_start`) is
//! enforced in-circuit as 32 byte-wise copy constraints per adjacency;
//! cross-snark continuity (last-snark-end == next-snark-start) and the
//! `BridgeEventFinalProof`↔hop-chain endpoint binding are enforced
//! off-circuit by `bundle_verifier` (§6 of the migration plan) and on-chain
//! by `AckiNackiBridge.withdrawByProofBundle` (§7).
//!
//! # Per-hop witness (`HopWitness`)
//!
//! Reused verbatim from [`crate::multi_hop_witness`]. Circuit consumes:
//! - `is_active: bool` — selector, `assert_bit`-checked
//! - `hop_start_block_id: [u8; 32]` — the *current* block-id (newer; the
//!   block whose `proof_block_refs` are being opened)
//! - `hop_end_block_id: [u8; 32]` — the *ref* block-id extracted from
//!   `hop_start_block_id`'s `proof_block_refs[ref_index]` (older)
//! - `block.block_id`, `block.block_merkle_tree_leaves[7]` — L7 and the block
//!   the L7 SHA walk lifts to (identical to `hop_start_block_id`)
//! - `block_merkle_leaf_proof_l7` — 4-sibling SHA-256 path for leaf 7
//! - `ref_index`, `refs_tree_depth`, `proof_block_ref_inner_path` — L7
//!   dense-merkle opening data
//!
//! # Constraints (per active hop)
//!
//! - `is_active ∈ {0, 1}` (`gate.assert_bit`).
//! - `ref_index ∈ (0, 2^MAX_PROOF_BLOCK_REFS_DEPTH)` via
//!   `walk_dense_merkle_bind_pos` (range-check + `num_to_bits`), plus
//!   `ref_index != 0` (spec §5.1 forbids opening slot 0, the same-thread
//!   parent).
//! - `refs_tree_depth ∈ [0, 16)` via 4-bit range check.
//! - **Ref-tree** — `ref_leaf = Poseidon([c0, c1, c2])` derived from the
//!   byte-flat `REFERENCED_REF_BLOCK_TAG (34 B) ‖ hop_end_block_id` layout
//!   (chunks 31+31+4). Walked to `computed_l7_fr` via variable-depth
//!   [`walk_dense_merkle_bind_pos`] over `MAX_PROOF_BLOCK_REFS_DEPTH = 8`
//!   levels. Gated equality: `(computed_l7_fr - l7_fr) · is_active == 0`.
//! - **SHA-256** — 4 compressions lift L7 to `block_id_bytes` at leaf index 7.
//!   Gated equality per byte: `(cur_bytes[i] - block_id_bytes[i]) · is_active
//!   == 0`.
//! - **Endpoint binding** — active hop enforces:
//!     - `hop_start_block_id[i] == block_id[i]`     (byte-wise, active-gated —
//!       the "current" newer block is the one whose L7 walk we just closed)
//!     - `hop_end_block_id[i]   == ref_block_id[i]` (byte-wise, active-gated —
//!       the older ref extracted from `proof_block_refs[ref_index]`)
//! - **Padding propagation** — inactive hop enforces:
//!     - `hop_start_block_id[i] == hop_end_block_id[i]` (byte-wise, gated by `1
//!       - is_active`)
//!
//! Intra-snark continuity: `hops[i].hop_end_block_id ==
//! hops[i+1].hop_start_block_id` for `i = 0..H_HOPS_PER_PROOF-1`, enforced
//! byte-wise unconditionally. In Direction (a) semantics: the older ref
//! extracted from hop `i` (its `hop_end`) is the current block being opened
//! by hop `i+1` (its `hop_start`).

use std::cell::RefCell;

use gosh_dense_balanced_tree::preprocess_dense_proof_padded;
use gosh_sha256_chip::Sha256Chip;
use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        flex_gate::MultiPhaseThreadBreakPoints,
        GateInstructions, RangeChip, RangeInstructions,
    },
    halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        halo2curves::{bn256::Fr, ff::Field as _},
        plonk::{Circuit, ConstraintSystem, Error},
    },
    poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher},
    AssignedValue, Context, QuantumCell,
};

use crate::{
    dense_merkle_bound::walk_dense_merkle_bind_pos,
    multi_hop_witness::{
        ref_leaf_hash_native, ref_leaf_ref_tag_chunk0_fr, ref_leaf_ref_tag_chunk1_lo_fr,
        BlockWitness, HopWitness, BLOCK_MERKLE_DEPTH, H_HOPS_PER_PROOF, MAX_PROOF_BLOCK_REFS_DEPTH,
    },
    poseidon::{RATE, R_F, R_P, T},
};

const SHA256_HASH_LEN: usize = 32;

/// Public-instance count: `[hop_start_block_id, hop_end_block_id]`.
pub const MULTI_HOP_PUBLIC_LEN: usize = 2;

#[derive(Clone, Debug)]
pub struct BridgeMultiHopProofConfig {
    base_circuit_config: BaseConfig<Fr>,
}

/// Cross-thread hop-chain snark. See module head for PI layout and per-hop
/// constraint list.
pub struct BridgeMultiHopProof {
    pub hops: [HopWitness; H_HOPS_PER_PROOF],
    pub base_circuit_params: BaseCircuitParams,
    pub base_circuit_builder: RefCell<BaseCircuitBuilder<Fr>>,
}

impl BridgeMultiHopProof {
    pub fn new(
        hops: [HopWitness; H_HOPS_PER_PROOF],
        base_circuit_params: BaseCircuitParams,
    ) -> Self {
        let base_circuit_builder = RefCell::new(
            BaseCircuitBuilder::<Fr>::new(false).use_params(base_circuit_params.clone()),
        );
        Self {
            hops,
            base_circuit_params,
            base_circuit_builder,
        }
    }

    pub fn new_for_proving(
        hops: [HopWitness; H_HOPS_PER_PROOF],
        base_circuit_params: BaseCircuitParams,
        break_points: MultiPhaseThreadBreakPoints,
    ) -> Self {
        let base_circuit_builder = RefCell::new(BaseCircuitBuilder::<Fr>::prover(
            base_circuit_params.clone(),
            break_points,
        ));
        Self {
            hops,
            base_circuit_params,
            base_circuit_builder,
        }
    }
}

// ============================================================================
// Per-hop gadgets (composition-style, mirrors DEX layout minus salted gadget)
// ============================================================================

/// Prove one hop's variable-depth L7 ref-tree opening.
///
/// Layers onto `ctx`:
/// * `ref_index != 0` (spec §5.1 — slot 0 is same-thread parent and never
///   opened as a hop edge).
/// * `ref_index ∈ [0, 2^MAX_PROOF_BLOCK_REFS_DEPTH)` via
///   `walk_dense_merkle_bind_pos`'s internal `range_check + num_to_bits`.
/// * `refs_tree_depth ∈ [0, 16)` via 4-bit range check.
/// * `ref_leaf_fr = Poseidon([c0, c1, c2])` from `REFERENCED_REF_BLOCK_TAG ‖
///   hop_end_block_id` (chunks 31+31+4). `hop_end_block_id` is the older
///   ref extracted from the current block's `proof_block_refs[ref_index]`.
/// * Variable-depth dense-merkle walk (`MAX_PROOF_BLOCK_REFS_DEPTH` levels,
///   gated per-level by `refs_tree_depth`) with direction bits bound to
///   `ref_index`'s bit-decomposition.
/// * Active-gated equality `(computed_l7_fr - l7_fr) · is_active == 0`.
///
/// Returns `(l7_bytes, ref_block_id_bytes)` — both 32-cell views, both
/// 8-bit range-checked. Downstream consumers: SHA-256 block-merkle walk
/// (`l7_bytes`) and endpoint binding (`ref_block_id_bytes`).
fn prove_hop_ref_tree_opening(
    ctx: &mut Context<Fr>,
    range: &RangeChip<Fr>,
    hasher: &PoseidonHasher<Fr, T, RATE>,
    hop: &HopWitness,
    is_active: AssignedValue<Fr>,
    ref_leaf_c0_const: AssignedValue<Fr>,
    ref_leaf_c1_tag_const: AssignedValue<Fr>,
    pow_256_3: AssignedValue<Fr>,
    powers_le_32: &[QuantumCell<Fr>],
) -> (Vec<AssignedValue<Fr>>, Vec<AssignedValue<Fr>>) {
    let gate = range.gate();

    // ref_index witness — must be nonzero (slot 0 = same-thread parent, spec §5.1).
    // The range check itself happens inside `walk_dense_merkle_bind_pos` below.
    let ref_index_assigned = ctx.load_witness(Fr::from(hop.ref_index as u64));
    {
        let is_zero_ref_index = gate.is_zero(ctx, ref_index_assigned);
        gate.assert_is_const(ctx, &is_zero_ref_index, &Fr::zero());
    }

    // L7 bytes + LE Fr packing.
    let l7_native = hop.block.block_merkle_tree_leaves[7];
    let l7_bytes: Vec<AssignedValue<Fr>> = l7_native
        .iter()
        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
        .collect();
    for cell in &l7_bytes {
        range.range_check(ctx, *cell, 8);
    }
    let l7_fr = {
        let cells: Vec<QuantumCell<Fr>> =
            l7_bytes.iter().map(|c| QuantumCell::Existing(*c)).collect();
        gate.inner_product(ctx, cells, powers_le_32[..32].iter().cloned())
    };

    // ref_block_id (== hop_end_block_id, semantically — the older block that
    // `proof_block_refs[ref_index]` points at) as 32 byte cells.
    let ref_block_id_bytes: Vec<AssignedValue<Fr>> = hop
        .hop_end_block_id
        .iter()
        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
        .collect();
    for cell in &ref_block_id_bytes {
        range.range_check(ctx, *cell, 8);
    }

    // Byte-flat ref-leaf chunks — ref-tag layout only (34 B tag): 31+31+4.
    //   c0 = tag_r_hi (31 B)               ← ref_leaf_c0_const
    //   c1 = tag_r_lo (3 B) + ref_block_id_lo28 · 256^3
    //   c2 = LE(ref_block_id[28..32])
    let ref_block_id_lo28 = {
        let cells: Vec<QuantumCell<Fr>> = ref_block_id_bytes[0..28]
            .iter()
            .map(|c| QuantumCell::Existing(*c))
            .collect();
        gate.inner_product(ctx, cells, powers_le_32[..28].iter().cloned())
    };
    let ref_block_id_lo28_shifted = gate.mul(
        ctx,
        QuantumCell::Existing(ref_block_id_lo28),
        QuantumCell::Existing(pow_256_3),
    );
    let ref_leaf_c1 = gate.add(
        ctx,
        QuantumCell::Existing(ref_leaf_c1_tag_const),
        QuantumCell::Existing(ref_block_id_lo28_shifted),
    );
    let ref_leaf_c2 = {
        let cells: Vec<QuantumCell<Fr>> = ref_block_id_bytes[28..32]
            .iter()
            .map(|c| QuantumCell::Existing(*c))
            .collect();
        gate.inner_product(ctx, cells, powers_le_32[..4].iter().cloned())
    };
    let ref_leaf_fr =
        hasher.hash_fix_len_array(ctx, gate, &[ref_leaf_c0_const, ref_leaf_c1, ref_leaf_c2]);

    // Byte-flat ref-tree walk — gated variable-depth fold.
    //
    // The chain's L7 tree width is `proof_block_refs.len().next_power_of_two()`
    // (spec §2.3, §5.2). We pass only the first `refs_tree_depth` real siblings
    // to the preprocessor; `preprocess_dense_proof_padded` synthesizes
    // identity-pair dummies for the remaining
    // `MAX_PROOF_BLOCK_REFS_DEPTH - refs_tree_depth` levels. In-circuit
    // `walk_dense_merkle_bind_pos` masks inactive levels via `gate.select`
    // keyed on `num_active_levels = refs_tree_depth`, so `computed_l7_fr`
    // equals the chain's variable-width root.
    let depth = hop.refs_tree_depth as usize;
    debug_assert!(
        depth <= MAX_PROOF_BLOCK_REFS_DEPTH,
        "refs_tree_depth {} > MAX_PROOF_BLOCK_REFS_DEPTH {}",
        depth,
        MAX_PROOF_BLOCK_REFS_DEPTH,
    );
    let ref_leaf_native_bytes = ref_leaf_hash_native(hop.ref_index, &hop.hop_end_block_id);
    let ref_proof = preprocess_dense_proof_padded(
        ref_leaf_native_bytes,
        &hop.proof_block_ref_inner_path[..depth],
        hop.ref_index,
        MAX_PROOF_BLOCK_REFS_DEPTH,
    );
    let refs_tree_depth_assigned = ctx.load_witness(Fr::from(hop.refs_tree_depth as u64));
    range.range_check(ctx, refs_tree_depth_assigned, 4);

    // Bound-direction-bit walker: does range_check(pos, max_depth) +
    // num_to_bits + zero-forcing loop + the walk itself.
    let (computed_l7_fr, _pos_bits) = walk_dense_merkle_bind_pos(
        ctx,
        range,
        hasher,
        &ref_proof,
        ref_leaf_fr,
        refs_tree_depth_assigned,
        ref_index_assigned,
        MAX_PROOF_BLOCK_REFS_DEPTH,
    );

    // Gated ref-tree root equality: (computed_l7_fr - l7_fr) * is_active == 0.
    {
        let diff = gate.sub(
            ctx,
            QuantumCell::Existing(computed_l7_fr),
            QuantumCell::Existing(l7_fr),
        );
        let gated = gate.mul(
            ctx,
            QuantumCell::Existing(diff),
            QuantumCell::Existing(is_active),
        );
        gate.assert_is_const(ctx, &gated, &Fr::zero());
    }

    (l7_bytes, ref_block_id_bytes)
}

/// Prove one hop's L7 → `block_id` SHA-256 walk (depth-4 merkle at leaf 7).
///
/// For leaf 7 in a 16-leaf depth-4 tree the sibling orientation is:
/// L0 (leaf level, index 7) — sibling on LEFT (7 is odd)
/// L1 (index 3) — sibling on LEFT (3 is odd)
/// L2 (index 1) — sibling on LEFT (1 is odd)
/// L3 (index 0) — sibling on RIGHT (0 is even)
/// So `cur` is on the RIGHT for the first 3 levels and on the LEFT for the
/// top level. Since leaf index is a compile-time constant, orientation is
/// constant per level.
///
/// Byte-wise gated equality against `block_id_bytes` closes the walk:
/// `(cur_bytes[i] - block_id_bytes[i]) · is_active == 0` for all 32 bytes.
fn prove_hop_block_merkle_sha256(
    ctx: &mut Context<Fr>,
    sha256_chip: &Sha256Chip<Fr>,
    gate: &impl GateInstructions<Fr>,
    l7_bytes: Vec<AssignedValue<Fr>>,
    block_id_bytes: &[AssignedValue<Fr>],
    block_merkle_leaf_proof_l7: &[[u8; 32]; BLOCK_MERKLE_DEPTH],
    is_active: AssignedValue<Fr>,
) {
    let mut cur_bytes = l7_bytes;
    for (level, sib_bytes) in block_merkle_leaf_proof_l7.iter().enumerate() {
        let sib_cells: Vec<AssignedValue<Fr>> = sib_bytes
            .iter()
            .map(|&b| ctx.load_witness(Fr::from(b as u64)))
            .collect();
        let mut concat: Vec<AssignedValue<Fr>> = Vec::with_capacity(64);
        // node_index at this level for leaf 7: 7 >> level.
        // Even → cur on left (cur ‖ sib); odd → cur on right (sib ‖ cur).
        let cur_on_right = ((7usize >> level) & 1) == 1;
        if cur_on_right {
            concat.extend_from_slice(&sib_cells);
            concat.extend_from_slice(&cur_bytes);
        } else {
            concat.extend_from_slice(&cur_bytes);
            concat.extend_from_slice(&sib_cells);
        }
        let next = sha256_chip.digest_bytes(ctx, &concat);
        assert_eq!(next.len(), SHA256_HASH_LEN);
        cur_bytes = next;
    }
    for i in 0..SHA256_HASH_LEN {
        let diff = gate.sub(
            ctx,
            QuantumCell::Existing(cur_bytes[i]),
            QuantumCell::Existing(block_id_bytes[i]),
        );
        let gated = gate.mul(
            ctx,
            QuantumCell::Existing(diff),
            QuantumCell::Existing(is_active),
        );
        gate.assert_is_const(ctx, &gated, &Fr::zero());
    }
}

/// Prove one hop's clear-byte endpoint bindings (bridge-side replacement for
/// DEX's `prove_hop_salted_endpoints`).
///
/// Direction (a) semantics: `hop_start` = *current block being opened*
/// (newer, == `block.block_id`); `hop_end` = *ref extracted from that block*
/// (older, == `proof_block_refs[ref_index]`).
///
/// Constraints (per byte, `i = 0..32`):
/// * `(hop_start_block_id[i] - block_id[i]) · is_active == 0` — active hop's
///   start endpoint is the current block whose L7 walk we just closed.
/// * `(hop_end_block_id[i] - ref_block_id[i]) · is_active == 0` — active
///   hop's end endpoint is the older L7-opened ref block_id.
/// * `(hop_start_block_id[i] - hop_end_block_id[i]) · not_active == 0` —
///   inactive padding hop collapses to a single terminal block-id, which the
///   intra-snark continuity chain then propagates through the tail.
///
/// Returns `(hop_start_bytes, hop_end_bytes)` — both 32-cell 8-bit
/// range-checked. Callers wire them into intra-snark continuity
/// (byte-wise `ctx.constrain_equal`).
#[allow(clippy::too_many_arguments)]
fn prove_hop_clear_endpoints(
    ctx: &mut Context<Fr>,
    range: &RangeChip<Fr>,
    hop: &HopWitness,
    block_id_bytes: &[AssignedValue<Fr>],
    ref_block_id_bytes: &[AssignedValue<Fr>],
    is_active: AssignedValue<Fr>,
    not_active: AssignedValue<Fr>,
) -> (Vec<AssignedValue<Fr>>, Vec<AssignedValue<Fr>>) {
    let gate = range.gate();

    let hop_start_bytes: Vec<AssignedValue<Fr>> = hop
        .hop_start_block_id
        .iter()
        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
        .collect();
    for cell in &hop_start_bytes {
        range.range_check(ctx, *cell, 8);
    }
    let hop_end_bytes: Vec<AssignedValue<Fr>> = hop
        .hop_end_block_id
        .iter()
        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
        .collect();
    for cell in &hop_end_bytes {
        range.range_check(ctx, *cell, 8);
    }

    for i in 0..32 {
        // Active-hop bindings (Direction (a) — start = newer current block,
        // end = older ref).
        let d_start = gate.sub(
            ctx,
            QuantumCell::Existing(hop_start_bytes[i]),
            QuantumCell::Existing(block_id_bytes[i]),
        );
        let g_start = gate.mul(
            ctx,
            QuantumCell::Existing(d_start),
            QuantumCell::Existing(is_active),
        );
        gate.assert_is_const(ctx, &g_start, &Fr::zero());

        let d_end = gate.sub(
            ctx,
            QuantumCell::Existing(hop_end_bytes[i]),
            QuantumCell::Existing(ref_block_id_bytes[i]),
        );
        let g_end = gate.mul(
            ctx,
            QuantumCell::Existing(d_end),
            QuantumCell::Existing(is_active),
        );
        gate.assert_is_const(ctx, &g_end, &Fr::zero());

        // Inactive-hop propagation.
        let d_pad = gate.sub(
            ctx,
            QuantumCell::Existing(hop_start_bytes[i]),
            QuantumCell::Existing(hop_end_bytes[i]),
        );
        let g_pad = gate.mul(
            ctx,
            QuantumCell::Existing(d_pad),
            QuantumCell::Existing(not_active),
        );
        gate.assert_is_const(ctx, &g_pad, &Fr::zero());
    }

    (hop_start_bytes, hop_end_bytes)
}

impl Circuit<Fr> for BridgeMultiHopProof {
    type Config = BridgeMultiHopProofConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = BaseCircuitParams;

    fn params(&self) -> Self::Params {
        self.base_circuit_params.clone()
    }

    fn without_witnesses(&self) -> Self {
        let dummy_hop = || HopWitness {
            is_active: false,
            block: BlockWitness {
                block_id: [0u8; 32],
                block_merkle_tree_leaves: [[0u8; 32]; 16],
                proof_block_refs: vec![[0u8; 32], [0u8; 32]],
            },
            block_merkle_leaf_proof_l7: [[0u8; 32]; BLOCK_MERKLE_DEPTH],
            // Slot 0 is same-thread parent — excluded from L7 walk (spec §5.1).
            // Even for inactive padding, ref_index must be ≥ 1 because the
            // in-circuit range + nonzero constraints on ref_index are
            // unconditional.
            ref_index: 1,
            // depth=1 keeps ref_index=1 within the live-flag window; the
            // walker's output is discarded by `is_active` anyway.
            refs_tree_depth: 1,
            proof_block_ref_inner_path: [[0u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH],
            hop_start_block_id: [0u8; 32],
            hop_end_block_id: [0u8; 32],
        };
        let hops: [HopWitness; H_HOPS_PER_PROOF] = std::array::from_fn(|_| dummy_hop());
        Self::new(hops, self.base_circuit_params.clone())
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        Self::configure_with_params(meta, Default::default())
    }

    fn configure_with_params(
        meta: &mut ConstraintSystem<Fr>,
        params: Self::Params,
    ) -> Self::Config {
        let base_circuit_config = BaseCircuitBuilder::<Fr>::configure_with_params(meta, params);
        BridgeMultiHopProofConfig {
            base_circuit_config,
        }
    }

    fn synthesize(&self, config: Self::Config, layouter: impl Layouter<Fr>) -> Result<(), Error> {
        {
            let old = self.base_circuit_builder.borrow();
            let mut fresh = if old.witness_gen_only() {
                BaseCircuitBuilder::<Fr>::prover(
                    self.base_circuit_params.clone(),
                    old.break_points(),
                )
            } else {
                BaseCircuitBuilder::<Fr>::new(false).use_params(self.base_circuit_params.clone())
            };
            while fresh.assigned_instances.len() < self.base_circuit_params.num_instance_columns {
                fresh.assigned_instances.push(vec![]);
            }
            drop(old);
            *self.base_circuit_builder.borrow_mut() = fresh;
        }

        let (first_start_fr, last_end_fr) = {
            let mut builder = self.base_circuit_builder.borrow_mut();
            let range = builder.range_chip();

            let (first_start_fr, last_end_fr) = {
                let gate = range.gate();
                let ctx = builder.pool(0).main();
                let sha256_chip = Sha256Chip::new(&range);

                let spec = OptimizedPoseidonSpec::<Fr, T, RATE>::new::<R_F, R_P, 0>();
                let mut hasher = PoseidonHasher::<Fr, T, RATE>::new(spec);
                hasher.initialize_consts(ctx, gate);

                // === Byte-flat constants (reused across all hops) ===
                // Ref-tag (34 B) chunk constants. Slot 0 (parent) is excluded
                // from the L7 walk (spec §5.1), so only the ref-tag layout is
                // used.
                let ref_leaf_c0_const = ctx.load_constant(ref_leaf_ref_tag_chunk0_fr());
                let ref_leaf_c1_tag_const = ctx.load_constant(ref_leaf_ref_tag_chunk1_lo_fr());
                let pow_256_3 = ctx.load_constant(Fr::from(256u64).pow([3u64]));
                // LE byte→Fr power-of-256 table, shared by every inner_product
                // in the per-hop loop (L7 pack, ref-leaf chunk math) and by
                // the head/tail Fr repack.
                let powers_le_32: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();

                let mut hop_endpoints: Vec<(Vec<AssignedValue<Fr>>, Vec<AssignedValue<Fr>>)> =
                    Vec::with_capacity(H_HOPS_PER_PROOF);

                for hop in self.hops.iter() {
                    // is_active + not_active flags for the whole hop.
                    let is_active =
                        ctx.load_witness(if hop.is_active { Fr::one() } else { Fr::zero() });
                    gate.assert_bit(ctx, is_active);
                    let one_const = ctx.load_constant(Fr::one());
                    let not_active = gate.sub(
                        ctx,
                        QuantumCell::Existing(one_const),
                        QuantumCell::Existing(is_active),
                    );

                    // Gadget 1: L7 ref-tree opening.
                    let (l7_bytes, ref_block_id_bytes) = prove_hop_ref_tree_opening(
                        ctx,
                        &range,
                        &hasher,
                        hop,
                        is_active,
                        ref_leaf_c0_const,
                        ref_leaf_c1_tag_const,
                        pow_256_3,
                        &powers_le_32,
                    );

                    // block_id byte witnesses (used by gadgets 2 and 3).
                    let block_id_bytes: Vec<AssignedValue<Fr>> = hop
                        .block
                        .block_id
                        .iter()
                        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
                        .collect();
                    for cell in &block_id_bytes {
                        range.range_check(ctx, *cell, 8);
                    }

                    // Gadget 2: L7 → block_id SHA-256 walk.
                    prove_hop_block_merkle_sha256(
                        ctx,
                        &sha256_chip,
                        gate,
                        l7_bytes,
                        &block_id_bytes,
                        &hop.block_merkle_leaf_proof_l7,
                        is_active,
                    );

                    // Gadget 3: clear-byte endpoint binding + padding
                    // propagation.
                    let (hop_start_bytes, hop_end_bytes) = prove_hop_clear_endpoints(
                        ctx,
                        &range,
                        hop,
                        &block_id_bytes,
                        &ref_block_id_bytes,
                        is_active,
                        not_active,
                    );

                    hop_endpoints.push((hop_start_bytes, hop_end_bytes));
                }

                // Intra-snark continuity: byte-wise copy constraint on every
                // adjacency (works uniformly across active/inactive
                // transitions).
                for i in 0..H_HOPS_PER_PROOF - 1 {
                    for b in 0..32 {
                        ctx.constrain_equal(&hop_endpoints[i].1[b], &hop_endpoints[i + 1].0[b]);
                    }
                }

                // Pack head-start and tail-end bytes into Fr (LE) for public
                // instances.
                let first_start_fr = {
                    let cells: Vec<QuantumCell<Fr>> = hop_endpoints[0]
                        .0
                        .iter()
                        .map(|c| QuantumCell::Existing(*c))
                        .collect();
                    gate.inner_product(ctx, cells, powers_le_32[..32].iter().cloned())
                };
                let last_end_fr = {
                    let cells: Vec<QuantumCell<Fr>> = hop_endpoints[H_HOPS_PER_PROOF - 1]
                        .1
                        .iter()
                        .map(|c| QuantumCell::Existing(*c))
                        .collect();
                    gate.inner_product(ctx, cells, powers_le_32[..32].iter().cloned())
                };

                (first_start_fr, last_end_fr)
            };

            builder.assigned_instances[0].push(first_start_fr);
            builder.assigned_instances[0].push(last_end_fr);
            (first_start_fr, last_end_fr)
        };

        let _ = (first_start_fr, last_end_fr);
        let builder = self.base_circuit_builder.borrow();
        builder.synthesize(config.base_circuit_config, layouter)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use gosh_dense_balanced_tree::bytes_to_fr;
    use halo2_base::{gates::circuit::BaseCircuitParams, halo2_proofs::dev::MockProver};

    use super::*;
    use crate::multi_hop_witness::{
        block_merkle_leaf_proof, block_merkle_root, proof_block_ref_inner_path_native,
        proof_block_refs_root_native, BLOCK_MERKLE_LEAF_COUNT,
    };

    /// Deterministic placeholder for the slot-0 same-thread parent — never
    /// opened by the circuit (spec §5.1) but must be a well-defined value
    /// so native `proof_block_refs_root_native` is reproducible.
    const SLOT0_PARENT_PLACEHOLDER: [u8; 32] = [0xF0; 32];

    /// Build one active hop opening `older_ref_id` (which will sit at slot 1
    /// of the current block's `proof_block_refs`) and derive the "current"
    /// block-id from that ref-tree + 16-leaf block-merkle. Returns the
    /// fully-populated `HopWitness` and the derived `current_block_id` —
    /// which is `hop.hop_start_block_id` under Direction (a) semantics
    /// (start = current/newer, end = older ref).
    fn make_active_hop(older_ref_id: [u8; 32], sentinel_byte: u8) -> (HopWitness, [u8; 32]) {
        let proof_block_refs: Vec<[u8; 32]> = vec![SLOT0_PARENT_PLACEHOLDER, older_ref_id];
        let l7 = proof_block_refs_root_native(&proof_block_refs);

        let mut leaves = [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT];
        for (j, slot) in leaves.iter_mut().enumerate().take(7) {
            *slot = [sentinel_byte; 32];
            slot[0] = j as u8;
        }
        leaves[7] = l7;

        let block_id = block_merkle_root(&leaves);
        let block_merkle_leaf_proof_l7 = block_merkle_leaf_proof(&leaves, 7);
        let ref_index = 1usize;
        let (proof_block_ref_inner_path, refs_tree_depth) =
            proof_block_ref_inner_path_native(&proof_block_refs, ref_index);

        let hop = HopWitness {
            is_active: true,
            block: BlockWitness {
                block_id,
                block_merkle_tree_leaves: leaves,
                proof_block_refs,
            },
            block_merkle_leaf_proof_l7,
            ref_index,
            refs_tree_depth,
            proof_block_ref_inner_path,
            hop_start_block_id: block_id,
            hop_end_block_id: older_ref_id,
        };
        (hop, block_id)
    }

    /// Build an inactive padding hop carrying `pad_bid` on both endpoints.
    /// `proof_block_refs` places `pad_bid` at slot 1 so native `l7`
    /// computation stays reproducible (the in-circuit fold is discarded via
    /// `is_active == 0`).
    fn make_inactive_hop(pad_bid: [u8; 32]) -> HopWitness {
        let proof_block_refs: Vec<[u8; 32]> = vec![SLOT0_PARENT_PLACEHOLDER, pad_bid];
        let (proof_block_ref_inner_path, refs_tree_depth) =
            proof_block_ref_inner_path_native(&proof_block_refs, 1);
        HopWitness {
            is_active: false,
            block: BlockWitness {
                block_id: pad_bid,
                block_merkle_tree_leaves: [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT],
                proof_block_refs,
            },
            block_merkle_leaf_proof_l7: [[0u8; 32]; BLOCK_MERKLE_DEPTH],
            ref_index: 1,
            refs_tree_depth,
            proof_block_ref_inner_path,
            hop_start_block_id: pad_bid,
            hop_end_block_id: pad_bid,
        }
    }

    /// Build `H_HOPS_PER_PROOF` hops from a `seed_bytes` (the *oldest* / anchor
    /// block-id `Y`) walking newest→oldest under Direction (a). We construct
    /// the chain oldest→newest (each iteration derives a newer block whose
    /// L7 references the previous older one) and then reverse so the first
    /// hop's `hop_start` is the newest / event block `X` and the last active
    /// hop's `hop_end` is `seed_bytes` (`Y`). Any tail is inactive padding
    /// carrying `Y` on both endpoints.
    fn synth_hops(seed_bytes: [u8; 32], k_active: usize) -> [HopWitness; H_HOPS_PER_PROOF] {
        assert!(k_active <= H_HOPS_PER_PROOF);
        let mut chain: Vec<HopWitness> = Vec::with_capacity(k_active);
        let mut older_bid = seed_bytes;
        for i in 0..k_active {
            let (hop, newer_bid) = make_active_hop(older_bid, 0x10 + i as u8);
            chain.push(hop);
            older_bid = newer_bid;
        }
        // Reverse so index 0 is the newest hop (start = X) and the last
        // active hop's end is `seed_bytes` (Y). At k_active=0 this is a
        // no-op and the tail pads with `seed_bytes`.
        chain.reverse();
        let terminal_older = if chain.is_empty() {
            seed_bytes
        } else {
            chain.last().unwrap().hop_end_block_id
        };
        while chain.len() < H_HOPS_PER_PROOF {
            chain.push(make_inactive_hop(terminal_older));
        }
        chain
            .try_into()
            .unwrap_or_else(|v: Vec<HopWitness>| panic!("hop slot count {}", v.len()))
    }

    fn test_params(k: usize) -> BaseCircuitParams {
        // At H_HOPS_PER_PROOF=1 the inner circuit is SHA-dominated (~2.83 M
        // cells for 8 compressions) — the same class as
        // `historical-layer-hashes-movement-checker-circuit`, which
        // `calculate_params` lands at 25 advice cols. See
        // `crates/bridge-circuits/docs/CIRCUIT_COMPLEXITY_COMPARISON.md` §2.
        //
        // MockProver min-col sweep (2026-09-27): 20 FAIL, 21 FAIL, 22 PASS,
        // 25 PASS, 30 PASS. Set to 25 for parity with LayerHashes and 3-col
        // safety margin (bytecode delta vs 22-col floor ≈ 1.4 KB, still
        // well under EIP-170).
        BaseCircuitParams {
            k,
            num_advice_per_phase: vec![25],
            num_fixed: 1,
            num_lookup_advice_per_phase: vec![9],
            lookup_bits: Some(16),
            num_instance_columns: 1,
        }
    }

    /// Positive: all `H_HOPS_PER_PROOF` hops active, chain fully packed.
    /// Exercises the ref-tree, SHA-256 block-merkle, and active endpoint
    /// bindings on every slot.
    #[test]
    fn all_active_hops_mock_prover() {
        let genesis: [u8; 32] = [0x01; 32];
        let hops = synth_hops(genesis, H_HOPS_PER_PROOF);
        let first_start = hops[0].hop_start_block_id;
        let last_end = hops[H_HOPS_PER_PROOF - 1].hop_end_block_id;

        // Sanity: chain continuity holds natively.
        for i in 0..H_HOPS_PER_PROOF - 1 {
            assert_eq!(hops[i].hop_end_block_id, hops[i + 1].hop_start_block_id);
        }

        const K: usize = 17;
        let params = test_params(K);
        let circuit = BridgeMultiHopProof::new(hops, params);
        let instances = vec![vec![bytes_to_fr(&first_start), bytes_to_fr(&last_end)]];
        let prover = MockProver::<Fr>::run(K as u32, &circuit, instances).unwrap();
        prover.assert_satisfied();
    }

    /// Positive with padding: 1 active + (H-1) inactive. Exercises the
    /// active↔inactive transition and the padding-hop identity rule. At H=1
    /// there is no padded slot to exercise — the test degenerates to the
    /// `all_active_hops_mock_prover` case and is skipped.
    #[test]
    fn one_active_rest_padded_mock_prover() {
        if H_HOPS_PER_PROOF < 2 {
            eprintln!(
                "skipping: H_HOPS_PER_PROOF={} < 2, no padded slot to exercise",
                H_HOPS_PER_PROOF,
            );
            return;
        }
        let genesis: [u8; 32] = [0x02; 32];
        let hops = synth_hops(genesis, 1);
        assert!(hops[0].is_active);
        for i in 1..H_HOPS_PER_PROOF {
            assert!(!hops[i].is_active);
            assert_eq!(hops[i].hop_start_block_id, hops[i].hop_end_block_id);
        }
        let first_start = hops[0].hop_start_block_id;
        let last_end = hops[H_HOPS_PER_PROOF - 1].hop_end_block_id;

        const K: usize = 17;
        let params = test_params(K);
        let circuit = BridgeMultiHopProof::new(hops, params);
        let instances = vec![vec![bytes_to_fr(&first_start), bytes_to_fr(&last_end)]];
        let prover = MockProver::<Fr>::run(K as u32, &circuit, instances).unwrap();
        prover.assert_satisfied();
    }

    /// Negative: publish a wrong `hop_end_block_id` PI. MockProver must fail.
    #[test]
    fn wrong_last_end_pi_fails() {
        let genesis: [u8; 32] = [0x03; 32];
        let hops = synth_hops(genesis, H_HOPS_PER_PROOF);
        let first_start = hops[0].hop_start_block_id;

        const K: usize = 17;
        let params = test_params(K);
        let circuit = BridgeMultiHopProof::new(hops, params);
        // Wrong last-end PI (all-ones instead of the real block_id).
        let bad_end: [u8; 32] = [0xFF; 32];
        let instances = vec![vec![bytes_to_fr(&first_start), bytes_to_fr(&bad_end)]];
        let prover = MockProver::<Fr>::run(K as u32, &circuit, instances).unwrap();
        assert!(
            prover.verify().is_err(),
            "mismatched last-end PI must fail MockProver"
        );
    }

    /// Negative: break intra-snark continuity by mutating the last hop's
    /// `hop_start_block_id` after building the chain. MockProver must fail
    /// (byte-wise `ctx.constrain_equal` on the adjacency edge fires).
    #[test]
    fn broken_adjacency_fails() {
        let genesis: [u8; 32] = [0x04; 32];
        let mut hops = synth_hops(genesis, H_HOPS_PER_PROOF);
        // Corrupt the last hop's hop_start_block_id — the adjacency edge
        // with the previous hop's hop_end_block_id no longer holds.
        let last = H_HOPS_PER_PROOF - 1;
        hops[last].hop_start_block_id[0] ^= 0x01;
        let first_start = hops[0].hop_start_block_id;
        let last_end = hops[H_HOPS_PER_PROOF - 1].hop_end_block_id;

        const K: usize = 17;
        let params = test_params(K);
        let circuit = BridgeMultiHopProof::new(hops, params);
        let instances = vec![vec![bytes_to_fr(&first_start), bytes_to_fr(&last_end)]];
        let prover = MockProver::<Fr>::run(K as u32, &circuit, instances).unwrap();
        assert!(
            prover.verify().is_err(),
            "broken adjacency must fail MockProver"
        );
    }

    /// Negative: inactive hop with `hop_start != hop_end` must fail (padding
    /// propagation rule violated). Requires an inactive slot; at H=1 the
    /// snark has no padding hop and the test is skipped.
    #[test]
    fn inactive_hop_endpoint_mismatch_fails() {
        if H_HOPS_PER_PROOF < 2 {
            eprintln!(
                "skipping: H_HOPS_PER_PROOF={} < 2, no padded slot to corrupt",
                H_HOPS_PER_PROOF,
            );
            return;
        }
        let genesis: [u8; 32] = [0x05; 32];
        // 1 active + (H-1) padded. First inactive slot is hops[1].
        let mut hops = synth_hops(genesis, 1);
        assert!(!hops[1].is_active);
        hops[1].hop_end_block_id[0] ^= 0x01;
        // We do NOT repair adjacency — either failure (padding rule OR
        // adjacency) is acceptable for this test.
        let first_start = hops[0].hop_start_block_id;
        let last_end = hops[H_HOPS_PER_PROOF - 1].hop_end_block_id;

        const K: usize = 17;
        let params = test_params(K);
        let circuit = BridgeMultiHopProof::new(hops, params);
        let instances = vec![vec![bytes_to_fr(&first_start), bytes_to_fr(&last_end)]];
        let prover = MockProver::<Fr>::run(K as u32, &circuit, instances).unwrap();
        assert!(
            prover.verify().is_err(),
            "inactive-hop endpoint mismatch must fail MockProver"
        );
    }
}
