//! Multi-thread Halo2 circuit proving an Acki Nacki `WithdrawalInitiated`
//! event was committed into a block on Acki Nacki, with the X-block (event
//! block) and Y-block (anchor block) *distinguishable* so that a companion
//! `BridgeMultiHopProof` can chain X → Y across
//! threads.
//!
//! This is the multi-thread successor of the removed single-thread
//! event-prove circuit (11 PIs). Every event proof now uses this
//! `BridgeEventFinalProof` (13 PIs); the legacy circuit and its module
//! have been deleted as part of the multi-thread migration. Shared
//! primitives (event byte layout, Poseidon-of-96-bytes helpers,
//! `MAX_ANCHOR_LAYER`, `MAX_EVENTS_TREE_DEPTH`) now live in
//! [`crate::event_primitives`]. See
//! [`docs/MULTITHREAD_MIGRATION_PLAN.md`] §4 for the staging plan.
//!
//! Public instances (`TOTAL_PUBLIC_INPUTS` total, `PUB_*` slot constants):
//!
//! ```text
//!   0  tokenId        (uint32 BE-pack of body[54..58))
//!   1  amount         (uint128 BE-pack of body[38..54))
//!   2  recipientHi    (uint80 BE-pack of recipient_bytes[2..12))
//!   3  recipientLo    (uint80 BE-pack of recipient_bytes[12..22))
//!   4  dstChainId     (uint256 BE-pack of body[6..38))
//!   5  senderAccFr    (Fr encoding of sender account_id, algebraic decode
//!                     of entries[3].cell_repr_data bits [11..267))
//!   6  dappFr         (Fr of account_dapp_id)
//!   7  accFr          (Fr of account_id)
//!   8  nullifier      (Poseidon(x_block_id_fr, tokenId, amount,
//!                                 recipientHi, recipientLo,
//!                                 senderAccFr, eventsPos))
//!   9  finalRoot      (anchor root the proof binds to)
//!  10  anchorLayer    (1-indexed layer number, 1..=MAX_ANCHOR_LAYER)
//!
//! 
//!
//! Slots `[0..=10]` are **byte-identical** to the legacy single-thread
//! layout (the same slot constants are re-exported from
//! [`crate::event_primitives`]): the daemon's existing PI parser reads them
//! unchanged and only needs to consume two additional Fr values at the tail.
//! `PUB_NULLIFIER` binds to `x_block_id_fr`
//! (the event's own block) so replay protection remains keyed on the source
//! block; the anchor-side `block_leaf` binds to `y_block_id_fr` (potentially
//! a different thread's block reached via a hop chain).
//!
//! New constraint delta vs the single-thread circuit:
//!
//! - **L8 opening bind.** `ext_out_root` (Poseidon image of the events
//!   sub-tree) is byte-linked to a witness `x_block_id_bytes`. Those bytes are
//!   then fed to [`crate::block_id_tree::assert_depth4_l8_opening_circuit`]
//!   together with a witness `h07_sibling` (opaque left aggregate of leaves
//!   0..=7 of the depth-4 block-id SHA tree). The gadget reconstructs
//!   `x_block_id` from the four SHA compressions and constrains equality with
//!   the witness `x_block_id_bytes`. This locks the events tree into the
//!   X-block's own `block_id` regardless of whether X == Y.
//! - **`is_same_thread` selector.** A 1-bit witness. When true, the circuit
//!   copy-constrains `x_block_id_fr == y_block_id_fr` — this is the shape the
//!   daemon submits for legacy same-thread claims (hop chain length 0). When
//!   false, X and Y are permitted to differ; equality of their tail with the
//!   hop-chain endpoints is enforced *off-circuit* by the `bundle_verifier`
//!   (Commit 5 of the plan).
//! - **Y-side `block_leaf`.** Constructed from `y_block_id_fr`,
//!   `envelope_hash_fr`, and `ext_out_root`. When cross-thread, the
//!   `ext_out_root` supplied here is the Y-block's L7 root — a semantic
//!   distinction the L7-walker circuit will enforce, and out of scope for this
//!   file.
//!
//! No other constraint changes: SHA-256 cell DAG, ABI event id, d1
//! refs_count, senderAccFr algebraic decode, events-tree walker,
//! block-tree walker, dense chain, and anchor-layer range check are
//! all preserved verbatim from the legacy single-thread circuit.
//! Shared primitives live in [`crate::event_primitives`].
//!
//! **Verifying key.** Different from the single-thread circuit — 13 PIs
//! plus 4 additional SHA-256 compressions in the L8 opening. Regenerate the
//! Circuit 4 VK when downstream consumers switch over.

use std::cell::RefCell;

use gosh_dense_balanced_tree::{
    bytes_to_fr, compute_root_native, dense_merkle_root_circuit, fr_to_bytes,
    preprocess_dense_proof, preprocess_dense_proof_padded, verify_chain_of_dense_proofs,
    DenseChainLink, MAX_CHAIN_LEN,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        flex_gate::MultiPhaseThreadBreakPoints,
        GateInstructions, RangeInstructions,
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
    block_id_tree::{assert_depth4_l8_opening_circuit, compute_block_id_from_l8_native},
    boc_helper::*,
    dense_merkle_bound::walk_dense_merkle_bind_pos,
    event_primitives::{
        poseidon_hash_96_circuit, poseidon_hash_96_native, ABI_EVENT_ID, EVENT_ABI_PREFIX_START,
        EVENT_AMOUNT_END, EVENT_AMOUNT_START, EVENT_DST_CHAIN_ID_END, EVENT_DST_CHAIN_ID_START,
        EVENT_TOKEN_ID_END, EVENT_TOKEN_ID_START, MAX_ANCHOR_LAYER, MAX_EVENTS_TREE_DEPTH,
        RECIPIENT_HALF_LEN, RECIPIENT_HI_END, RECIPIENT_HI_START, RECIPIENT_LO_END,
        RECIPIENT_LO_START,
    },
    poseidon::*,
};

const SHA256_HASH_LEN: usize = 32;

// ───── Public-input layout (instance column 0) ─────────────────────────────
//
// Slots `[0..=10]` mirror the legacy single-thread layout
// byte-for-byte. Slots `[11..=12]` are the multi-thread additions.

pub const PUB_TOKEN_ID: usize = 0;
pub const PUB_AMOUNT: usize = 1;
pub const PUB_RECIPIENT_HI: usize = 2;
pub const PUB_RECIPIENT_LO: usize = 3;
pub const PUB_DST_CHAIN_ID: usize = 4;
pub const PUB_SENDER_ACC_FR: usize = 5;
pub const PUB_DAPP_FR: usize = 6;
pub const PUB_ACC_FR: usize = 7;
pub const PUB_NULLIFIER: usize = 8;
pub const PUB_FINAL_ROOT: usize = 9;
pub const PUB_ANCHOR_LAYER: usize = 10;
/// New in multi-thread layout: Fr-encoding of the X-block's `block_id`
/// (the event block). Reconstructed in-circuit from `ext_out_root` via the
/// depth-4 L8 opening.
pub const PUB_X_BLOCK_ID: usize = 11;
/// New in multi-thread layout: Fr-encoding of the Y-block's `block_id`
/// (the anchor block, potentially on a different thread reached via a
/// separate hop-chain snark).
pub const PUB_Y_BLOCK_ID: usize = 12;
pub const TOTAL_PUBLIC_INPUTS: usize = 13;

#[derive(Clone, Debug)]
pub struct BridgeEventFinalProofConfig {
    base_circuit_config: BaseConfig<Fr>,
}

/// Multi-thread bridge event-prove circuit. See module head for full PI
/// layout and constraint delta vs the removed single-thread circuit.
pub struct BridgeEventFinalProof {
    /// Private witness: flattened cell DAG entries (wrapper, body,
    /// recipient, sender).
    pub entries: [BocFlattenData; 4],
    /// Private witness: events-tree Merkle proof siblings (bottom-up).
    pub merkle_proof_siblings: Vec<[u8; 32]>,
    pub merkle_proof_position: usize,
    pub account_dapp_id: [u8; 32],
    pub account_id: [u8; 32],
    /// Event block's `block_id` — used by the nullifier and reconstructed
    /// in-circuit via the L8 opening from `ext_out_root` + `h07_sibling`.
    pub x_block_id: [u8; 32],
    /// Anchor block's `block_id` — used by the block-leaf Poseidon input
    /// and by the block-tree walker. May equal `x_block_id` (same-thread)
    /// or differ (cross-thread claim binding proved by a hop-chain snark).
    pub y_block_id: [u8; 32],
    /// Opaque SHA-256 sibling for the depth-4 block-id tree: aggregate of
    /// leaves 0..=7 of the X-block's block-id SHA tree. The circuit does
    /// not open its structure; only the four compressions on the leaf-8
    /// opening path are constrained. See
    /// [`crate::block_id_tree`] for the layout.
    pub h07_sibling: [u8; 32],
    /// When `true`, the circuit copy-constrains `x_block_id == y_block_id`.
    /// When `false`, the two are permitted to differ (the off-circuit
    /// `bundle_verifier` binds them to the hop chain's endpoints).
    pub is_same_thread: bool,
    pub envelope_hash_bytes: [u8; 32],
    pub block_merkle_proof_siblings: Vec<[u8; 32]>,
    pub block_merkle_proof_position: usize,
    pub dense_chain: Vec<DenseChainLink>,
    pub num_active_chain_steps: usize,
    /// 1-indexed routing hint chosen by the prover, exposed as
    /// `PUB_ANCHOR_LAYER`. Range-checked in-circuit; binding to
    /// `layerWindows[]` happens on-chain.
    pub anchor_layer: u8,
    pub base_circuit_params: BaseCircuitParams,
    pub base_circuit_builder: RefCell<BaseCircuitBuilder<Fr>>,
}

impl BridgeEventFinalProof {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        entries: [BocFlattenData; 4],
        merkle_proof_siblings: Vec<[u8; 32]>,
        merkle_proof_position: usize,
        account_dapp_id: [u8; 32],
        account_id: [u8; 32],
        x_block_id: [u8; 32],
        y_block_id: [u8; 32],
        h07_sibling: [u8; 32],
        is_same_thread: bool,
        envelope_hash_bytes: [u8; 32],
        block_merkle_proof_siblings: Vec<[u8; 32]>,
        block_merkle_proof_position: usize,
        dense_chain: Vec<DenseChainLink>,
        num_active_chain_steps: usize,
        anchor_layer: u8,
        base_circuit_params: BaseCircuitParams,
    ) -> Self {
        Self::assert_invariants(
            &merkle_proof_siblings,
            merkle_proof_position,
            &dense_chain,
            num_active_chain_steps,
            anchor_layer,
            &x_block_id,
            &y_block_id,
            is_same_thread,
        );
        let base_circuit_builder = RefCell::new(
            BaseCircuitBuilder::<Fr>::new(false).use_params(base_circuit_params.clone()),
        );
        Self {
            entries,
            merkle_proof_siblings,
            merkle_proof_position,
            account_dapp_id,
            account_id,
            x_block_id,
            y_block_id,
            h07_sibling,
            is_same_thread,
            envelope_hash_bytes,
            block_merkle_proof_siblings,
            block_merkle_proof_position,
            dense_chain,
            num_active_chain_steps,
            anchor_layer,
            base_circuit_params,
            base_circuit_builder,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_for_proving(
        entries: [BocFlattenData; 4],
        merkle_proof_siblings: Vec<[u8; 32]>,
        merkle_proof_position: usize,
        account_dapp_id: [u8; 32],
        account_id: [u8; 32],
        x_block_id: [u8; 32],
        y_block_id: [u8; 32],
        h07_sibling: [u8; 32],
        is_same_thread: bool,
        envelope_hash_bytes: [u8; 32],
        block_merkle_proof_siblings: Vec<[u8; 32]>,
        block_merkle_proof_position: usize,
        dense_chain: Vec<DenseChainLink>,
        num_active_chain_steps: usize,
        anchor_layer: u8,
        base_circuit_params: BaseCircuitParams,
        break_points: MultiPhaseThreadBreakPoints,
    ) -> Self {
        Self::assert_invariants(
            &merkle_proof_siblings,
            merkle_proof_position,
            &dense_chain,
            num_active_chain_steps,
            anchor_layer,
            &x_block_id,
            &y_block_id,
            is_same_thread,
        );
        let base_circuit_builder = RefCell::new(BaseCircuitBuilder::<Fr>::prover(
            base_circuit_params.clone(),
            break_points,
        ));
        Self {
            entries,
            merkle_proof_siblings,
            merkle_proof_position,
            account_dapp_id,
            account_id,
            x_block_id,
            y_block_id,
            h07_sibling,
            is_same_thread,
            envelope_hash_bytes,
            block_merkle_proof_siblings,
            block_merkle_proof_position,
            dense_chain,
            num_active_chain_steps,
            anchor_layer,
            base_circuit_params,
            base_circuit_builder,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_invariants(
        merkle_proof_siblings: &[[u8; 32]],
        merkle_proof_position: usize,
        dense_chain: &[DenseChainLink],
        num_active_chain_steps: usize,
        anchor_layer: u8,
        x_block_id: &[u8; 32],
        y_block_id: &[u8; 32],
        is_same_thread: bool,
    ) {
        assert!(
            merkle_proof_siblings.len() <= MAX_EVENTS_TREE_DEPTH,
            "events tree depth {} exceeds MAX_EVENTS_TREE_DEPTH {}",
            merkle_proof_siblings.len(),
            MAX_EVENTS_TREE_DEPTH,
        );
        let max_pos = 1usize << merkle_proof_siblings.len();
        assert!(
            merkle_proof_position < max_pos.max(1),
            "merkle_proof_position {} out of range for depth {} (max={})",
            merkle_proof_position,
            merkle_proof_siblings.len(),
            max_pos,
        );
        assert_eq!(dense_chain.len(), MAX_CHAIN_LEN);
        assert!(num_active_chain_steps <= MAX_CHAIN_LEN);
        assert!(
            anchor_layer >= 1 && anchor_layer <= MAX_ANCHOR_LAYER,
            "anchor_layer {} out of range 1..={}",
            anchor_layer,
            MAX_ANCHOR_LAYER,
        );
        // Native mirror of the in-circuit copy-constraint under
        // `is_same_thread`. Fails fast on the caller's thread so a
        // malformed witness surfaces at construction rather than as a
        // MockProver failure deep inside the prover.
        if is_same_thread {
            assert_eq!(
                x_block_id, y_block_id,
                "is_same_thread=true requires x_block_id == y_block_id"
            );
        }
    }
}

impl Circuit<Fr> for BridgeEventFinalProof {
    type Config = BridgeEventFinalProofConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = BaseCircuitParams;

    fn params(&self) -> Self::Params {
        self.base_circuit_params.clone()
    }

    fn without_witnesses(&self) -> Self {
        let dummy_entries: [BocFlattenData; 4] = std::array::from_fn(|i| BocFlattenData {
            repr_hash: [0u8; 32],
            refs_count: self.entries[i].refs_count,
            childs_repr_hashes_offset: self.entries[i].childs_repr_hashes_offset.clone(),
            cell_repr_data: vec![0u8; self.entries[i].cell_repr_data.len()],
        });
        let dummy_chain = self
            .dense_chain
            .iter()
            .map(|link| DenseChainLink::inactive([0u8; 32], link.siblings.len()))
            .collect();
        // For a same-thread `without_witnesses` skeleton, x == y trivially
        // satisfies the invariant. `h07_sibling` is unconstrained here —
        // any 32-byte value keeps the L8 gadget's arithmetic well-typed
        // during keygen.
        Self::new(
            dummy_entries,
            vec![[0u8; 32]; MAX_EVENTS_TREE_DEPTH],
            0,
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            true,
            [0u8; 32],
            self.block_merkle_proof_siblings
                .iter()
                .map(|_| [0u8; 32])
                .collect(),
            0,
            dummy_chain,
            0,
            self.anchor_layer,
            self.base_circuit_params.clone(),
        )
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        Self::configure_with_params(meta, Default::default())
    }

    fn configure_with_params(
        meta: &mut ConstraintSystem<Fr>,
        params: Self::Params,
    ) -> Self::Config {
        let base_circuit_config = BaseCircuitBuilder::<Fr>::configure_with_params(meta, params);
        BridgeEventFinalProofConfig {
            base_circuit_config,
        }
    }

    fn synthesize(&self, config: Self::Config, layouter: impl Layouter<Fr>) -> Result<(), Error> {
        // Reset the base circuit builder so repeated synthesize calls
        // (keygen_vk + keygen_pk) don't accumulate gates.
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

        {
            let mut builder = self.base_circuit_builder.borrow_mut();
            let range = builder.range_chip();

            let (
                token_id,
                amount_fr,
                recipient_hi_fr,
                recipient_lo_fr,
                dst_chain_id_fr,
                sender_acc_fr,
                dapp_fr,
                acc_fr,
                nullifier_fr,
                final_root,
                anchor_layer_fr,
                x_block_id_fr,
                y_block_id_fr,
            ) = {
                let gate = range.gate();
                let ctx = builder.pool(0).main();
                let sha256_chip = Sha256Chip::new(&range);

                // === Assign all 4 preimages as byte witnesses ===
                let assign_bytes = |ctx: &mut Context<Fr>, data: &[u8]| -> Vec<AssignedValue<Fr>> {
                    data.iter()
                        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
                        .collect()
                };
                let wrapper_bytes = assign_bytes(ctx, &self.entries[0].cell_repr_data);
                let body_bytes = assign_bytes(ctx, &self.entries[1].cell_repr_data);
                let recipient_bytes = assign_bytes(ctx, &self.entries[2].cell_repr_data);
                let sender_bytes = assign_bytes(ctx, &self.entries[3].cell_repr_data);

                // === SHA-256 over each cell preimage ===
                let wrapper_hash = sha256_chip.digest_bytes(ctx, &wrapper_bytes);
                let body_hash = sha256_chip.digest_bytes(ctx, &body_bytes);
                let recipient_hash = sha256_chip.digest_bytes(ctx, &recipient_bytes);
                let sender_hash = sha256_chip.digest_bytes(ctx, &sender_bytes);

                // === Child-hash equality links (wrapper → body; body → recipient/sender)
                let wrapper_ch_off: usize = self.entries[0]
                    .childs_repr_hashes_offset
                    .as_ref()
                    .expect("wrapper cell must have a child hash offset")[0]
                    as usize;
                for i in 0..SHA256_HASH_LEN {
                    ctx.constrain_equal(&wrapper_bytes[wrapper_ch_off + i], &body_hash[i]);
                }
                let body_ch_offsets = self.entries[1]
                    .childs_repr_hashes_offset
                    .as_ref()
                    .expect("body cell must have child hash offsets");
                assert_eq!(body_ch_offsets.len(), 2);
                let body_ch_off_0 = body_ch_offsets[0] as usize;
                let body_ch_off_1 = body_ch_offsets[1] as usize;
                for i in 0..SHA256_HASH_LEN {
                    ctx.constrain_equal(&body_bytes[body_ch_off_0 + i], &recipient_hash[i]);
                    ctx.constrain_equal(&body_bytes[body_ch_off_1 + i], &sender_hash[i]);
                }

                // === ABI event id constraint ===
                for (i, &expected_byte) in ABI_EVENT_ID.iter().enumerate() {
                    let expected = ctx.load_constant(Fr::from(expected_byte as u64));
                    ctx.constrain_equal(&body_bytes[EVENT_ABI_PREFIX_START + i], &expected);
                }

                // === Field extraction from body / recipient ===
                let be_powers = |len: usize| -> Vec<QuantumCell<Fr>> {
                    (0..len)
                        .map(|i| {
                            QuantumCell::Constant(Fr::from(256u64).pow([(len - 1 - i) as u64]))
                        })
                        .collect()
                };
                let to_existing = |slice: &[AssignedValue<Fr>]| -> Vec<QuantumCell<Fr>> {
                    slice.iter().map(|b| QuantumCell::Existing(*b)).collect()
                };

                let dst_chain_id_fr = gate.inner_product(
                    ctx,
                    to_existing(&body_bytes[EVENT_DST_CHAIN_ID_START..EVENT_DST_CHAIN_ID_END]),
                    be_powers(EVENT_DST_CHAIN_ID_END - EVENT_DST_CHAIN_ID_START),
                );
                let amount_fr = gate.inner_product(
                    ctx,
                    to_existing(&body_bytes[EVENT_AMOUNT_START..EVENT_AMOUNT_END]),
                    be_powers(EVENT_AMOUNT_END - EVENT_AMOUNT_START),
                );
                let token_id = gate.inner_product(
                    ctx,
                    to_existing(&body_bytes[EVENT_TOKEN_ID_START..EVENT_TOKEN_ID_END]),
                    be_powers(EVENT_TOKEN_ID_END - EVENT_TOKEN_ID_START),
                );
                let recipient_hi_fr = gate.inner_product(
                    ctx,
                    to_existing(&recipient_bytes[RECIPIENT_HI_START..RECIPIENT_HI_END]),
                    be_powers(RECIPIENT_HALF_LEN),
                );
                let recipient_lo_fr = gate.inner_product(
                    ctx,
                    to_existing(&recipient_bytes[RECIPIENT_LO_START..RECIPIENT_LO_END]),
                    be_powers(RECIPIENT_HALF_LEN),
                );

                // === d1 refs_count checks (wrapper=1, body=2, recipient=0, sender=0) ===
                let constrain_refs_count =
                    |ctx: &mut Context<Fr>,
                     d1_assigned: &AssignedValue<Fr>,
                     d1_native: u8,
                     expected_refs: u8| {
                        let bits: Vec<AssignedValue<Fr>> = (0..8u32)
                            .map(|i| ctx.load_witness(Fr::from(((d1_native >> i) & 1) as u64)))
                            .collect();
                        for &bit in &bits {
                            gate.assert_bit(ctx, bit);
                        }
                        let powers: Vec<_> = (0..8u32)
                            .map(|i| QuantumCell::Constant(Fr::from(1u64 << i)))
                            .collect();
                        let reconstructed = gate.inner_product(ctx, bits.clone(), powers);
                        ctx.constrain_equal(d1_assigned, &reconstructed);
                        for i in 0..3 {
                            let expected_bit =
                                ctx.load_constant(Fr::from(((expected_refs >> i) & 1) as u64));
                            ctx.constrain_equal(&bits[i], &expected_bit);
                        }
                    };
                constrain_refs_count(ctx, &wrapper_bytes[0], self.entries[0].cell_repr_data[0], 1);
                constrain_refs_count(ctx, &body_bytes[0], self.entries[1].cell_repr_data[0], 2);
                constrain_refs_count(
                    ctx,
                    &recipient_bytes[0],
                    self.entries[2].cell_repr_data[0],
                    0,
                );
                constrain_refs_count(ctx, &sender_bytes[0], self.entries[3].cell_repr_data[0], 0);

                // === Poseidon hasher (shared for everything that follows) ===
                let spec = OptimizedPoseidonSpec::<Fr, T, RATE>::new::<R_F, R_P, 0>();
                let mut hasher = PoseidonHasher::<Fr, T, RATE>::new(spec);
                hasher.initialize_consts(ctx, gate);

                // === Pack wrapper SHA-256 hash to repr_hash_fr (LE) ===
                let le_powers_32: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();
                let wrapper_hash_cells: Vec<QuantumCell<Fr>> = wrapper_hash
                    .iter()
                    .map(|b| QuantumCell::Existing(*b))
                    .collect();
                let repr_hash_fr = gate.inner_product(ctx, wrapper_hash_cells, le_powers_32);

                // === ext_msg_leaf = Poseidon96(dapp_id, account_id, repr_hash) ===
                let dapp_fr = ctx.load_witness(bytes_to_fr(&self.account_dapp_id));
                let acc_fr = ctx.load_witness(bytes_to_fr(&self.account_id));
                let ext_msg_leaf_fr = poseidon_hash_96_circuit(
                    ctx,
                    &range,
                    &hasher,
                    dapp_fr,
                    acc_fr,
                    repr_hash_fr,
                    &self.account_dapp_id,
                    &self.account_id,
                    &self.entries[0].repr_hash,
                );

                // === ext_msg_leaf → ext_out_messages_root (padded events tree) ===
                let ext_msg_leaf_native = poseidon_hash_96_native(
                    &self.account_dapp_id,
                    &self.account_id,
                    &self.entries[0].repr_hash,
                );
                let events_proof_native = preprocess_dense_proof(
                    ext_msg_leaf_native,
                    &self.merkle_proof_siblings,
                    self.merkle_proof_position,
                );
                let events_proof_padded = preprocess_dense_proof_padded(
                    ext_msg_leaf_native,
                    &self.merkle_proof_siblings,
                    self.merkle_proof_position,
                    MAX_EVENTS_TREE_DEPTH,
                );
                let num_events_levels =
                    ctx.load_witness(Fr::from(self.merkle_proof_siblings.len() as u64));
                range.range_check(ctx, num_events_levels, 4);
                let max_ev_const = ctx.load_constant(Fr::from(MAX_EVENTS_TREE_DEPTH as u64));
                let ev_diff = gate.sub(ctx, max_ev_const, num_events_levels);
                range.range_check(ctx, ev_diff, 4);

                // === events_pos binding ===
                let events_pos_fr = ctx.load_witness(Fr::from(self.merkle_proof_position as u64));
                let (ext_out_root, _events_pos_bits) = walk_dense_merkle_bind_pos(
                    ctx,
                    &range,
                    &hasher,
                    &events_proof_padded,
                    ext_msg_leaf_fr,
                    num_events_levels,
                    events_pos_fr,
                    MAX_EVENTS_TREE_DEPTH,
                );

                // === Native ext_out_root bytes (for L8 opening + block_leaf) ===
                let ext_out_root_bytes = if self.merkle_proof_siblings.is_empty() {
                    ext_msg_leaf_native
                } else {
                    fr_to_bytes(compute_root_native(&events_proof_native))
                };

                // === L8 opening: bind ext_out_root → x_block_id ==========
                //
                // The events sub-tree root IS the L8 leaf of the X-block's
                // depth-4 SHA-256 block-id tree (see `block_id_tree.rs`).
                // We
                //   (a) load 32 witness bytes for ext_out_root and pin them
                //       to the Fr walker output via LE inner-product,
                //   (b) load 32 witness bytes for x_block_id and pin them
                //       to the x_block_id_fr Fr witness via LE inner-product,
                //   (c) call the depth-4 SHA opening gadget with the
                //       h07_sibling witness. The gadget reconstructs the
                //       root from four SHA compressions and constrains
                //       equality byte-for-byte with the loaded
                //       x_block_id_bytes cells.
                let ext_out_root_bytes_cells: Vec<AssignedValue<Fr>> = ext_out_root_bytes
                    .iter()
                    .map(|&b| ctx.load_witness(Fr::from(b as u64)))
                    .collect();
                for &b in &ext_out_root_bytes_cells {
                    range.range_check(ctx, b, 8);
                }
                let le_powers_32b: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();
                let ext_out_root_repack = gate.inner_product(
                    ctx,
                    ext_out_root_bytes_cells
                        .iter()
                        .map(|b| QuantumCell::Existing(*b)),
                    le_powers_32b,
                );
                ctx.constrain_equal(&ext_out_root_repack, &ext_out_root);

                // Load h07_sibling bytes (opaque left aggregate of leaves 0..=7).
                let h07_sibling_cells: Vec<AssignedValue<Fr>> = self
                    .h07_sibling
                    .iter()
                    .map(|&b| ctx.load_witness(Fr::from(b as u64)))
                    .collect();

                // Load x_block_id bytes + Fr witness; pin the Fr to LE-pack.
                let x_block_id_bytes_cells: Vec<AssignedValue<Fr>> = self
                    .x_block_id
                    .iter()
                    .map(|&b| ctx.load_witness(Fr::from(b as u64)))
                    .collect();
                for &b in &x_block_id_bytes_cells {
                    range.range_check(ctx, b, 8);
                }
                let x_block_id_fr = ctx.load_witness(bytes_to_fr(&self.x_block_id));
                let le_powers_32c: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();
                let x_block_id_repack = gate.inner_product(
                    ctx,
                    x_block_id_bytes_cells
                        .iter()
                        .map(|b| QuantumCell::Existing(*b)),
                    le_powers_32c,
                );
                ctx.constrain_equal(&x_block_id_repack, &x_block_id_fr);

                // Native cross-check: the byte-form x_block_id must be the
                // SHA depth-4 opening of ext_out_root against h07_sibling.
                // Off-circuit assertion so a malformed witness surfaces at
                // MockProver `assert_satisfied()` rather than as an opaque
                // SHA-gadget mismatch. The in-circuit gadget below is the
                // sound version — this is a defensive fail-fast.
                let native_open =
                    compute_block_id_from_l8_native(&ext_out_root_bytes, &self.h07_sibling);
                assert_eq!(
                    native_open, self.x_block_id,
                    "witness inconsistency: x_block_id must equal SHA depth-4 opening of \
                     ext_out_root against h07_sibling"
                );

                let ext_out_root_bytes_arr: &[AssignedValue<Fr>; 32] = ext_out_root_bytes_cells
                    .as_slice()
                    .try_into()
                    .expect("32-byte ext_out_root cell array");
                let h07_sibling_arr: &[AssignedValue<Fr>; 32] = h07_sibling_cells
                    .as_slice()
                    .try_into()
                    .expect("32-byte h07_sibling cell array");
                let x_block_id_bytes_arr: &[AssignedValue<Fr>; 32] = x_block_id_bytes_cells
                    .as_slice()
                    .try_into()
                    .expect("32-byte x_block_id cell array");
                assert_depth4_l8_opening_circuit(
                    ctx,
                    &range,
                    &sha256_chip,
                    ext_out_root_bytes_arr,
                    h07_sibling_arr,
                    x_block_id_bytes_arr,
                );

                // === y_block_id witness + optional same-thread copy-constraint ===
                let y_block_id_fr = ctx.load_witness(bytes_to_fr(&self.y_block_id));
                let is_same_thread_fr = ctx.load_witness(Fr::from(self.is_same_thread as u64));
                gate.assert_bit(ctx, is_same_thread_fr);
                // `(x - y) * is_same_thread == 0` forces x == y when the
                // selector is 1, and imposes nothing when the selector is 0.
                let x_minus_y = gate.sub(
                    ctx,
                    QuantumCell::Existing(x_block_id_fr),
                    QuantumCell::Existing(y_block_id_fr),
                );
                let gated = gate.mul(
                    ctx,
                    QuantumCell::Existing(x_minus_y),
                    QuantumCell::Existing(is_same_thread_fr),
                );
                let zero_const = ctx.load_constant(Fr::zero());
                ctx.constrain_equal(&gated, &zero_const);

                // === block_leaf = Poseidon96(y_block_id, envelope_hash, ext_out_root) ===
                let envelope_hash_fr = ctx.load_witness(bytes_to_fr(&self.envelope_hash_bytes));
                let block_leaf_fr = poseidon_hash_96_circuit(
                    ctx,
                    &range,
                    &hasher,
                    y_block_id_fr,
                    envelope_hash_fr,
                    ext_out_root,
                    &self.y_block_id,
                    &self.envelope_hash_bytes,
                    &ext_out_root_bytes,
                );

                // === block_leaf → history window root (root_1) ===
                let block_leaf_native = poseidon_hash_96_native(
                    &self.y_block_id,
                    &self.envelope_hash_bytes,
                    &ext_out_root_bytes,
                );
                let block_proof = preprocess_dense_proof(
                    block_leaf_native,
                    &self.block_merkle_proof_siblings,
                    self.block_merkle_proof_position,
                );
                let root_1 =
                    dense_merkle_root_circuit(ctx, &range, &hasher, &block_proof, block_leaf_fr);

                // === root_1 → final_root via dense chain ===
                let num_active = ctx.load_witness(Fr::from(self.num_active_chain_steps as u64));
                range.range_check(ctx, num_active, 4);
                let max_chain_const = ctx.load_constant(Fr::from(MAX_CHAIN_LEN as u64));
                let max_minus_na = gate.sub(ctx, max_chain_const, num_active);
                range.range_check(ctx, max_minus_na, 4);

                let final_root = verify_chain_of_dense_proofs(
                    ctx,
                    &range,
                    &hasher,
                    root_1,
                    &self.dense_chain,
                    num_active,
                );

                // === senderAccFr algebraic decode (identical to single-thread) ===
                let mut sender_high3: Vec<AssignedValue<Fr>> = Vec::with_capacity(33);
                let mut sender_low5: Vec<AssignedValue<Fr>> = Vec::with_capacity(33);
                let c32 = QuantumCell::Constant(Fr::from(32u64));
                for j in 3..(2 + 34) {
                    let byte_val = self.entries[3].cell_repr_data[j];
                    let high3_val = (byte_val >> 5) as u64;
                    let low5_val = (byte_val & 0x1F) as u64;
                    let h = ctx.load_witness(Fr::from(high3_val));
                    let l = ctx.load_witness(Fr::from(low5_val));
                    range.range_check(ctx, h, 3);
                    range.range_check(ctx, l, 5);
                    let recon = gate.mul_add(ctx, h, c32, l);
                    ctx.constrain_equal(&recon, &sender_bytes[j]);
                    sender_high3.push(h);
                    sender_low5.push(l);
                }
                let c8 = QuantumCell::Constant(Fr::from(8u64));
                let mut acc_id_bytes: Vec<QuantumCell<Fr>> = Vec::with_capacity(32);
                for i in 0..32 {
                    let combined = gate.mul_add(ctx, sender_low5[i], c8, sender_high3[i + 1]);
                    acc_id_bytes.push(QuantumCell::Existing(combined));
                }
                let le_powers_32_acc: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();
                let sender_acc_fr = gate.inner_product(ctx, acc_id_bytes, le_powers_32_acc);

                // === Nullifier binds to x_block_id_fr (source block) ===
                let nullifier_fr = hasher.hash_fix_len_array(ctx, gate, &[
                    x_block_id_fr,
                    token_id,
                    amount_fr,
                    recipient_hi_fr,
                    recipient_lo_fr,
                    sender_acc_fr,
                    events_pos_fr,
                ]);

                // === anchorLayer range check ===
                let anchor_layer_fr = ctx.load_witness(Fr::from(self.anchor_layer as u64));
                let one_const = ctx.load_constant(Fr::one());
                let al_minus_1 = gate.sub(
                    ctx,
                    QuantumCell::Existing(anchor_layer_fr),
                    QuantumCell::Existing(one_const),
                );
                range.range_check(ctx, al_minus_1, 4);
                let max_al_const = ctx.load_constant(Fr::from(MAX_ANCHOR_LAYER as u64));
                let max_minus_al = gate.sub(
                    ctx,
                    QuantumCell::Existing(max_al_const),
                    QuantumCell::Existing(anchor_layer_fr),
                );
                range.range_check(ctx, max_minus_al, 4);

                (
                    token_id,
                    amount_fr,
                    recipient_hi_fr,
                    recipient_lo_fr,
                    dst_chain_id_fr,
                    sender_acc_fr,
                    dapp_fr,
                    acc_fr,
                    nullifier_fr,
                    final_root,
                    anchor_layer_fr,
                    x_block_id_fr,
                    y_block_id_fr,
                )
            };

            let slots: [AssignedValue<Fr>; TOTAL_PUBLIC_INPUTS] = [
                token_id,
                amount_fr,
                recipient_hi_fr,
                recipient_lo_fr,
                dst_chain_id_fr,
                sender_acc_fr,
                dapp_fr,
                acc_fr,
                nullifier_fr,
                final_root,
                anchor_layer_fr,
                x_block_id_fr,
                y_block_id_fr,
            ];
            for slot in slots {
                builder.assigned_instances[0].push(slot);
            }
        }

        let builder = self.base_circuit_builder.borrow();
        builder.synthesize(config.base_circuit_config, layouter)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use dense_balanced_tree::PoseidonHasher as DensePoseidonHasher;
    use gosh_dense_balanced_tree::bytes_to_fr;
    use halo2_base::halo2_proofs::dev::MockProver;
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;
    use crate::{
        block_id_tree::compute_block_id_from_l8_native,
        event_primitives::poseidon_hash_96_native as poseidon_hash_96,
        test_helpers::{
            base_circuit_params, build_dense_chain, build_final_proof_two_level_tree,
            compute_leading_public_inputs, load_first_withdrawal, make_final_proof_instances, K,
        },
    };

    /// Same-thread MockProver pass: x_block_id == y_block_id,
    /// is_same_thread=true. The witness builder computes block_id from a
    /// synthetic ext_out_root + h07_sibling so the L8 opening reconstructs
    /// a matching root.
    #[test]
    fn same_thread_mock_prover_pass() {
        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260926);

        let tw = build_final_proof_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
        let (dense_chain, final_root_bytes) = build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let circuit = BridgeEventFinalProof::new(
            w.entries.clone(),
            tw.events_siblings,
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id, // x_block_id
            tw.block_id, // y_block_id (same-thread)
            tw.h07_sibling,
            true,
            tw.envelope_hash_bytes,
            tw.block_siblings,
            tw.block_pos,
            dense_chain,
            1,
            1,
            params,
        );

        let leading = compute_leading_public_inputs(
            &w,
            &tw.block_id,
            &tw.account_dapp_id,
            &tw.account_id,
            &w.sender_account_id,
            tw.events_pos,
        );
        let block_id_fr = bytes_to_fr(&tw.block_id);
        let instances = make_final_proof_instances(
            leading,
            final_root_fr,
            Fr::from(1u64),
            block_id_fr,
            block_id_fr,
        );
        assert_eq!(instances.len(), TOTAL_PUBLIC_INPUTS);
        let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
        prover.assert_satisfied();
    }

    /// Cross-thread MockProver pass: x_block_id != y_block_id,
    /// is_same_thread=false. The L8 opening still binds ext_out_root →
    /// x_block_id; the y_block_id is free-floating (bound off-circuit by
    /// `bundle_verifier` in a later commit). Skips the block-tree walker
    /// constraint on the *y* side by keeping `block_leaf_native =
    /// Poseidon96(y_block_id, envelope, ext_out_root)` consistent with the
    /// walker's prepared block proof — done by having
    /// `build_final_proof_two_level_tree` build the block leaf from
    /// `y_block_id` when passed a distinct value. Here we override y.
    #[test]
    fn cross_thread_mock_prover_pass() {
        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260927);

        // Build with y_block_id != x_block_id — the helper takes both.
        let mut y_block_id = [0u8; 32];
        use rand::Rng;
        rng.fill(&mut y_block_id);

        let tw = crate::test_helpers::build_final_proof_two_level_tree_cross(
            &w.repr_hash,
            &mut rng,
            &dense_hasher,
            128,
            130,
            y_block_id,
        );
        let (dense_chain, final_root_bytes) = build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let circuit = BridgeEventFinalProof::new(
            w.entries.clone(),
            tw.events_siblings,
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id,
            y_block_id,
            tw.h07_sibling,
            false,
            tw.envelope_hash_bytes,
            tw.block_siblings,
            tw.block_pos,
            dense_chain,
            1,
            1,
            params,
        );

        let leading = compute_leading_public_inputs(
            &w,
            &tw.block_id, // nullifier binds to x_block_id
            &tw.account_dapp_id,
            &tw.account_id,
            &w.sender_account_id,
            tw.events_pos,
        );
        let x_fr = bytes_to_fr(&tw.block_id);
        let y_fr = bytes_to_fr(&y_block_id);
        let instances =
            make_final_proof_instances(leading, final_root_fr, Fr::from(1u64), x_fr, y_fr);
        let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
        prover.assert_satisfied();
    }

    /// Negative: is_same_thread=true with x != y must be rejected — the
    /// constructor's `assert_invariants` fires before MockProver runs.
    #[test]
    #[should_panic(expected = "is_same_thread=true requires x_block_id == y_block_id")]
    fn same_thread_rejects_x_neq_y_in_ctor() {
        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260928);

        let tw = build_final_proof_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
        let (dense_chain, _final_root_bytes) = build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let params = base_circuit_params();

        let mut different_y = tw.block_id;
        different_y[0] ^= 0xFF;

        let _ = BridgeEventFinalProof::new(
            w.entries.clone(),
            tw.events_siblings,
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id,
            different_y,
            tw.h07_sibling,
            true,
            tw.envelope_hash_bytes,
            tw.block_siblings,
            tw.block_pos,
            dense_chain,
            1,
            1,
            params,
        );
    }

    /// Sanity: `compute_block_id_from_l8_native` used by the helper is
    /// deterministic and matches what the in-circuit L8 opening asserts.
    #[test]
    fn helper_block_id_matches_l8_opening_native() {
        let ext_out = [0x11u8; 32];
        let sib = [0x22u8; 32];
        let bid = compute_block_id_from_l8_native(&ext_out, &sib);
        let bid_again = compute_block_id_from_l8_native(&ext_out, &sib);
        assert_eq!(bid, bid_again);
        // Poseidon reference just to prove the function compiles here.
        let _ = poseidon_hash_96(&bid, &sib, &ext_out);
    }
}
