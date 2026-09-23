//! Halo2 circuit proving an Acki Nacki `WithdrawalInitiated` event was
//! committed into a block on Acki Nacki.
//!
//! Public instances (`TOTAL_PUBLIC_INPUTS` total, see [`PUB_*`] index
//! constants):
//!   0: tokenId        — BE-pack of body[54..58)              (uint32)
//!   1: amount         — BE-pack of body[38..54)              (uint128)
//!   2: recipientHi    — BE-pack of recipient_bytes[2..12)    (uint80 hi)
//!   3: recipientLo    — BE-pack of recipient_bytes[12..22)   (uint80 lo)
//!   4: dstChainId     — BE-pack of body[6..38)               (uint256)
//!   5: senderAccFr    — Fr-encoding of the sender's 256-bit
//!                       `account_id`, algebraically decoded from
//!                       `entries[3].cell_repr_data` bits [11..267)
//!   6: dappFr         — Fr-encoding of account_dapp_id  (binds proof to dApp)
//!   7: accFr          — Fr-encoding of account_id       (binds proof to account)
//!   8: nullifier      — Poseidon(block_id_fr, tokenId, amount,
//!                                  recipientHi, recipientLo,
//!                                  senderAccFr, eventsPos)
//!                       `eventsPos` disambiguates two identical
//!                       `WithdrawalInitiated` events in the same AN block.
//!                       It is a PRIVATE witness — soundness comes from
//!                       binding its bit-decomposition to the events-tree
//!                       merkle path via
//!                       [`dense_merkle_bound::walk_dense_merkle_bind_pos`],
//!                       so the value hashed IS the position walked.
//!   9: finalRoot      — Anchor root the proof binds to. The verifier
//!                       checks `finalRoot` against the layer window named
//!                       by `anchorLayer` off-circuit. The bridge has no
//!                       anonymization goal, so the prover exposes a
//!                       single anchor root rather than the previous
//!                       `NUM_LAYER_HASHES`-wide candidate vector with
//!                       a private index.
//!  10: anchorLayer    — 1-indexed routing hint chosen by the prover.
//!                       Only range-checked (`1..=MAX_ANCHOR_LAYER`) and
//!                       forwarded to slot 10; roots inside the walk carry
//!                       no layer tag. Identity is enforced on-chain by
//!                       `AckiNackiBridge.sol` scanning
//!                       `layerWindows[anchorLayer]` for `finalRoot`, so a
//!                       wrong hint just fails that membership check. 0 is
//!                       reserved as "invalid/unset".
//!
//! Why no `senderDappFr`: the TVM address type (`std_addr$10`, see
//! `MsgAddrStd { anycast, workchain_id, address }` in
//! `tvm-sdk/tvm_block/src/messages.rs`) carries **no** dApp-id field — only
//! the workchain and the 256-bit `account_id`. The `dapp_id` lives in
//! account state (`ShardAccount { …, dapp_id: Option<UInt256> }` in
//! `tvm-sdk/tvm_block/src/accounts.rs`), so binding it would require a
//! separate `ShardAccount`-state Merkle proof — substantially more work,
//! out of scope for v2. If the destination side ever needs the sender's
//! dApp-id, the right architectural fix is for the AN bridge contract dev
//! to add a `senderDappId` field directly to the `WithdrawalInitiated`
//! event so it appears in the body cell BOC and can be parsed/bound the
//! same way as `tokenId` / `dstChainId`.
//!
//! Recipient split convention: α (10/10 BE bytes, see §4.1 of the v2 plan and
//! `EVENT_LAYOUT_COMPARISON.md`). β (uint128+uint32) and γ (uint80+uint80)
//! are numerically equivalent 1-line variants.
//!
//! Private witnesses include the recipient cell bytes and the sender cell
//! bytes. They are bound to the event by the SHA-256 cell-tree verification
//! and (for recipient) algebraically linked to recipientHi/recipientLo.
//!
//! Cell DAG (4 cells in BFS order, see EVENT_LAYOUT_COMPARISON.md):
//!   entries[0] — ExtOut wrapper (refs=1 → entries[1])
//!   entries[1] — event body cell (refs=2 → entries[2], entries[3])
//!   entries[2] — recipient cell  (refs=0, fixed 22-byte cell_repr_data for
//!                                  20-byte Ethereum addresses)
//!   entries[3] — sender cell     (refs=0, fixed 36-byte cell_repr_data,
//!                                  std_addr$10 + workchain=0 + 256-bit acc_id)
//!
//! Pipeline (single base-circuit region, mirrors `DarkDexCircuitNew`):
//!  1. SHA-256 of all 4 preimages via `gosh-sha256-chip`.
//!  2. Three child-hash equality links: wrapper→body, body→recipient,
//!     body→sender.
//!  3. Extract `tokenId`, `amount`, `dstChainId` from
//!     `entries[1].cell_repr_data` (all PUBLIC in v2). Extract
//!     `recipientHi`/`recipientLo` from `entries[2].cell_repr_data`.
//!  4. Constrain ABI event id == 0x3c838959 at body bytes [2..6).
//!  5. Constrain `d1` refs_count of each cell (1, 2, 0, 0).
//!  6. Pack root SHA-256 hash to `repr_hash_fr`.
//!  7. `ext_msg_leaf = Poseidon96(dapp_id, account_id, repr_hash)`.
//!  8. Verify `ext_msg_leaf -> ext_out_root` via padded events Merkle proof.
//!  9. `block_leaf = Poseidon96(block_id, envelope_hash, ext_out_root)`.
//! 10. Verify `block_leaf -> root_1` via block Merkle proof.
//! 11. Verify `root_1 -> final_root` via dense chain (`verify_chain_of_dense_proofs`).
//! 12. `nullifier = Poseidon(block_id_fr, tokenId, amount, recipientHi,
//!     recipientLo, senderAccFr, eventsPos)` via `hash_fix_len_array`.
//!     `eventsPos` is bound to the events-tree walker's direction bits so
//!     two identical `WithdrawalInitiated` events in one AN block yield
//!     distinct nullifiers.
//! 13. Push public instances in the [`PUB_*`] order — leading fields,
//!     `nullifier`, `final_root`, then `anchorLayer` (1-indexed,
//!     range-checked `1..=MAX_ANCHOR_LAYER`) — to instance column 0.

use gosh_sha256_chip::Sha256Chip;
use halo2_base::halo2_proofs::halo2curves::ff::Field as _;
use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner},
    halo2curves::bn256::Fr,
    plonk::{Circuit, ConstraintSystem, Error},
};

use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        flex_gate::MultiPhaseThreadBreakPoints,
        GateInstructions, RangeInstructions,
    },
    poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher},
    AssignedValue, Context, QuantumCell,
};

use std::cell::RefCell;

use crate::boc_helper::*;
use crate::dense_merkle_bound::walk_dense_merkle_bind_pos;
use crate::poseidon::*;
use gosh_dense_balanced_tree::{
    bytes_to_fr, compute_root_native, dense_merkle_root_circuit, fr_to_bytes,
    poseidon_hash_native, preprocess_dense_proof, preprocess_dense_proof_padded,
    verify_chain_of_dense_proofs, DenseChainLink, MAX_CHAIN_LEN,
};

pub const MAX_EVENTS_TREE_DEPTH: usize = 8;

/// Maximum 1-indexed layer number for `PUB_ANCHOR_LAYER`. Must equal the
/// Solidity `MAX_LAYER_HASHES` in `AckiNackiBridge.sol` (currently 10). Any
/// change here MUST land together with the on-chain constant — the verifier
/// range-checks `pub.anchorLayer > MAX_LAYER_HASHES` and reverts with
/// `InvalidNumLayers(numLayers)`, so a circuit that emits
/// `anchorLayer > 10` would produce proofs the bridge rejects with that
/// specific selector (not silently).
pub const MAX_ANCHOR_LAYER: u8 = 10;

// ───── Public-input layout (instance column 0) ─────────────────────────────
//
// Total = `TOTAL_PUBLIC_INPUTS` Fr values. Slot indices are exposed as named
// constants so downstream consumers (verifier daemon, Solidity verifier
// glue) can index without magic numbers.

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
pub const TOTAL_PUBLIC_INPUTS: usize = 11;

/// First-cut hardcoded recipient length — Ethereum addresses are 20 bytes.
/// All 10 captured fixtures match this. To support variable lengths,
/// see TODO §5.6 in `EVENT_LAYOUT_COMPARISON.md`.
pub const RECIPIENT_LEN_FIXED: usize = 20;

const SHA256_HASH_LEN: usize = 32;

// ───── Event body byte layout (cell_repr_data-relative offsets) ────────────
//
// The body cell (entries[1]) is exactly 126 bytes:
//   [0..2)    d1 + d2
//   [2..6)    ABI event id  (0x3c838959 for WithdrawalInitiated)
//   [6..38)   dstChainId    (uint256, 32 B, BE)              ← private
//   [38..54)  amount        (uint128, 16 B, BE)              ← private
//   [54..58)  tokenId       (uint32,   4 B, BE)              ← public
//   [58..62)  child_depths  (2 × u16 BE)
//   [62..94)  child_hash[0] = sha256(recipient cell)
//   [94..126) child_hash[1] = sha256(sender cell)
pub const EVENT_ABI_PREFIX_START: usize = 2;
pub const EVENT_ABI_PREFIX_END: usize = 6;
const EVENT_BOC_DATA_BYTES_OFFSET: usize = 6;
const EVENT_DST_CHAIN_ID_FIELD_LEN: usize = 32;
const EVENT_AMOUNT_FIELD_LEN: usize = 16;
const EVENT_TOKEN_ID_FIELD_LEN: usize = 4;

pub const EVENT_DST_CHAIN_ID_START: usize = EVENT_BOC_DATA_BYTES_OFFSET;
pub const EVENT_DST_CHAIN_ID_END: usize = EVENT_DST_CHAIN_ID_START + EVENT_DST_CHAIN_ID_FIELD_LEN;
pub const EVENT_AMOUNT_START: usize = EVENT_DST_CHAIN_ID_END;
pub const EVENT_AMOUNT_END: usize = EVENT_AMOUNT_START + EVENT_AMOUNT_FIELD_LEN;
pub const EVENT_TOKEN_ID_START: usize = EVENT_AMOUNT_END;
pub const EVENT_TOKEN_ID_END: usize = EVENT_TOKEN_ID_START + EVENT_TOKEN_ID_FIELD_LEN;

/// ABI event id of `WithdrawalInitiated` (first 4 bytes of the truncated
/// SHA-256 of the canonical signature, per TVM Solidity ABI v2).
pub const ABI_EVENT_ID: [u8; 4] = [0x3c, 0x83, 0x89, 0x59];

/// Fixed byte length of the body cell's `cell_repr_data`.
pub const BODY_CELL_LEN: usize = 126;

/// Fixed byte length of the recipient cell's `cell_repr_data`
/// (when `recipient.length == RECIPIENT_LEN_FIXED`).
pub const RECIPIENT_CELL_LEN: usize = 2 + RECIPIENT_LEN_FIXED;

// Recipient cell `cell_repr_data` layout:
//   [0..2)   d1 + d2
//   [2..22)  20 raw recipient address bytes
//
// v2 split α (default): hi = BE-pack(bytes[2..12)), lo = BE-pack(bytes[12..22)).
// Each half holds 80 bits (uint80). ETH reassembly:
//   address(uint160((uint256(hi) << 80) | uint256(lo)))
//
// TODO(circuit4-v2): alternative split conventions if ETH side prefers
// different on-chain typing:
//   β = uint128(bytes[0..16]) + uint32(bytes[16..20]) — 4 leading zero bytes
//       (wasteful — top 4 bytes of `recipientHi` are always 0)
//   γ = uint80(bytes[0..10])  + uint80(bytes[10..20]) — identical to α
pub const RECIPIENT_DATA_OFFSET: usize = 2;
pub const RECIPIENT_HALF_LEN: usize = RECIPIENT_LEN_FIXED / 2; // = 10
pub const RECIPIENT_HI_START: usize = RECIPIENT_DATA_OFFSET;
pub const RECIPIENT_HI_END: usize = RECIPIENT_HI_START + RECIPIENT_HALF_LEN;
pub const RECIPIENT_LO_START: usize = RECIPIENT_HI_END;
pub const RECIPIENT_LO_END: usize = RECIPIENT_LO_START + RECIPIENT_HALF_LEN;

/// Fixed byte length of the sender cell's `cell_repr_data`
/// (267-bit std_addr → 34 byte payload).
pub const SENDER_CELL_LEN: usize = 2 + 34;

// ───── Poseidon-of-96-bytes helpers (verbatim from dark-dex) ───────────────

/// Split 96 bytes at 31-byte boundaries into 4 LE Fr elements.
fn chunk_96_bytes_to_fr(buf: &[u8; 96]) -> (Fr, Fr, Fr, Fr) {
    let mut b0 = [0u8; 32];
    b0[..31].copy_from_slice(&buf[0..31]);
    let c0 = bytes_to_fr(&b0);

    let mut b1 = [0u8; 32];
    b1[..31].copy_from_slice(&buf[31..62]);
    let c1 = bytes_to_fr(&b1);

    let mut b2 = [0u8; 32];
    b2[..31].copy_from_slice(&buf[62..93]);
    let c2 = bytes_to_fr(&b2);

    let mut b3 = [0u8; 32];
    b3[..3].copy_from_slice(&buf[93..96]);
    let c3 = bytes_to_fr(&b3);

    (c0, c1, c2, c3)
}

/// Native: Poseidon hash of 3 × 32-byte inputs, chunked at 31-byte boundaries.
pub fn poseidon_hash_96_native(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(a);
    buf[32..64].copy_from_slice(b);
    buf[64..96].copy_from_slice(c);
    let (c0, c1, c2, c3) = chunk_96_bytes_to_fr(&buf);
    let hash = poseidon_hash_native(&[c0, c1, c2, c3]);
    fr_to_bytes(hash)
}

/// In-circuit: Poseidon hash of 3 × 32-byte inputs with algebraic linking.
/// Identical to dark-dex's `poseidon_hash_96_circuit`.
fn poseidon_hash_96_circuit(
    ctx: &mut Context<Fr>,
    range: &impl RangeInstructions<Fr>,
    hasher: &PoseidonHasher<Fr, T, RATE>,
    a_fr: AssignedValue<Fr>,
    b_fr: AssignedValue<Fr>,
    c_fr: AssignedValue<Fr>,
    a_bytes: &[u8; 32],
    b_bytes: &[u8; 32],
    c_bytes: &[u8; 32],
) -> AssignedValue<Fr> {
    let gate = range.gate();

    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(a_bytes);
    buf[32..64].copy_from_slice(b_bytes);
    buf[64..96].copy_from_slice(c_bytes);
    let (c0_val, _c1_val, _c2_val, c3_val) = chunk_96_bytes_to_fr(&buf);

    let hi_a_val = Fr::from(a_bytes[31] as u64);

    let mut low_b_buf = [0u8; 32];
    low_b_buf[..30].copy_from_slice(&b_bytes[..30]);
    let low_b_val = bytes_to_fr(&low_b_buf);

    let hi_b_val = Fr::from(b_bytes[30] as u64 + (b_bytes[31] as u64) * 256);

    let mut low_c_buf = [0u8; 32];
    low_c_buf[..29].copy_from_slice(&c_bytes[..29]);
    let low_c_val = bytes_to_fr(&low_c_buf);

    let c0 = ctx.load_witness(c0_val);
    let hi_a = ctx.load_witness(hi_a_val);
    let low_b = ctx.load_witness(low_b_val);
    let hi_b = ctx.load_witness(hi_b_val);
    let low_c = ctx.load_witness(low_c_val);
    let c3 = ctx.load_witness(c3_val);

    let pow_248 = QuantumCell::Constant(Fr::from(2u64).pow([248]));
    let sum_a = gate.mul_add(ctx, hi_a, pow_248, c0);
    ctx.constrain_equal(&sum_a, &a_fr);

    let c256 = QuantumCell::Constant(Fr::from(256u64));
    let c1 = gate.mul_add(ctx, low_b, c256, hi_a);

    let pow_240 = QuantumCell::Constant(Fr::from(2u64).pow([240]));
    let sum_b = gate.mul_add(ctx, hi_b, pow_240, low_b);
    ctx.constrain_equal(&sum_b, &b_fr);

    let pow_16 = QuantumCell::Constant(Fr::from(1u64 << 16));
    let c2 = gate.mul_add(ctx, low_c, pow_16, hi_b);

    let pow_232 = QuantumCell::Constant(Fr::from(2u64).pow([232]));
    let sum_c = gate.mul_add(ctx, c3, pow_232, low_c);
    ctx.constrain_equal(&sum_c, &c_fr);

    range.range_check(ctx, c0, 248);
    range.range_check(ctx, hi_a, 8);
    range.range_check(ctx, low_b, 240);
    range.range_check(ctx, hi_b, 16);
    range.range_check(ctx, low_c, 232);
    range.range_check(ctx, c3, 24);

    hasher.hash_fix_len_array(ctx, gate, &[c0, c1, c2, c3])
}

/// BE-pack a byte slice into an Fr (off-circuit). For sanity-checking instance
/// values from parsed BOCs.
pub fn be_bytes_to_fr(bytes: &[u8]) -> Fr {
    let mut v = Fr::from(0u64);
    for &b in bytes {
        v = v * Fr::from(256u64) + Fr::from(b as u64);
    }
    v
}

#[derive(Clone, Debug)]
pub struct BridgeEventProveCircuitConfig {
    base_circuit_config: BaseConfig<Fr>,
}

pub struct BridgeEventProveCircuit {
    /// Private witness: flattened cell DAG entries (wrapper, body, recipient, sender).
    pub entries: [BocFlattenData; 4],
    /// Private witness: events-tree Merkle proof siblings (bottom-up).
    pub merkle_proof_siblings: Vec<[u8; 32]>,
    pub merkle_proof_position: usize,
    pub account_dapp_id: [u8; 32],
    pub account_id: [u8; 32],
    pub block_id: [u8; 32],
    pub envelope_hash_bytes: [u8; 32],
    pub block_merkle_proof_siblings: Vec<[u8; 32]>,
    pub block_merkle_proof_position: usize,
    pub dense_chain: Vec<DenseChainLink>,
    pub num_active_chain_steps: usize,
    /// 1-indexed layer routing hint chosen by the prover, exposed as
    /// `PUB_ANCHOR_LAYER`. Range-checked in-circuit; binding to
    /// `layerWindows[]` happens on-chain.
    pub anchor_layer: u8,
    pub base_circuit_params: BaseCircuitParams,
    pub base_circuit_builder: RefCell<BaseCircuitBuilder<Fr>>,
}

impl BridgeEventProveCircuit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        entries: [BocFlattenData; 4],
        merkle_proof_siblings: Vec<[u8; 32]>,
        merkle_proof_position: usize,
        account_dapp_id: [u8; 32],
        account_id: [u8; 32],
        block_id: [u8; 32],
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
            block_id,
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
        block_id: [u8; 32],
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
            block_id,
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

    fn assert_invariants(
        merkle_proof_siblings: &[[u8; 32]],
        merkle_proof_position: usize,
        dense_chain: &[DenseChainLink],
        num_active_chain_steps: usize,
        anchor_layer: u8,
    ) {
        assert!(
            merkle_proof_siblings.len() <= MAX_EVENTS_TREE_DEPTH,
            "events tree depth {} exceeds MAX_EVENTS_TREE_DEPTH {}",
            merkle_proof_siblings.len(),
            MAX_EVENTS_TREE_DEPTH,
        );
        // Native mirror of the in-circuit `range_check(events_pos, MAX_EVENTS_TREE_DEPTH)`
        // + zero-on-inactive-bits constraints. Fails fast on invalid witnesses so
        // the divergence is caught at construction rather than as a MockProver
        // constraint violation.
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
    }
}

impl Circuit<Fr> for BridgeEventProveCircuit {
    type Config = BridgeEventProveCircuitConfig;
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
        Self::new(
            dummy_entries,
            vec![[0u8; 32]; MAX_EVENTS_TREE_DEPTH],
            0,
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            self.block_merkle_proof_siblings.iter().map(|_| [0u8; 32]).collect(),
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
        BridgeEventProveCircuitConfig { base_circuit_config }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        // Reset the base circuit builder so that repeated synthesize calls
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
                // digest_bytes range-checks every input byte to [0, 255].
                let wrapper_hash = sha256_chip.digest_bytes(ctx, &wrapper_bytes);
                let body_hash = sha256_chip.digest_bytes(ctx, &body_bytes);
                let recipient_hash = sha256_chip.digest_bytes(ctx, &recipient_bytes);
                let sender_hash = sha256_chip.digest_bytes(ctx, &sender_bytes);

                // === Child-hash equality links (3 links, 96 byte-equalities) ===

                // Link 1: wrapper → body (entries[0].child_hashes[0] == sha256(body))
                let wrapper_ch_off: usize = self.entries[0]
                    .childs_repr_hashes_offset
                    .as_ref()
                    .expect("wrapper cell must have a child hash offset")[0]
                    as usize;
                for i in 0..SHA256_HASH_LEN {
                    ctx.constrain_equal(&wrapper_bytes[wrapper_ch_off + i], &body_hash[i]);
                }

                // Link 2 & 3: body → recipient, body → sender
                let body_ch_offsets = self.entries[1]
                    .childs_repr_hashes_offset
                    .as_ref()
                    .expect("body cell must have child hash offsets");
                assert_eq!(
                    body_ch_offsets.len(),
                    2,
                    "body cell must have exactly 2 child hash offsets"
                );
                let body_ch_off_0 = body_ch_offsets[0] as usize;
                let body_ch_off_1 = body_ch_offsets[1] as usize;
                for i in 0..SHA256_HASH_LEN {
                    ctx.constrain_equal(&body_bytes[body_ch_off_0 + i], &recipient_hash[i]);
                    ctx.constrain_equal(&body_bytes[body_ch_off_1 + i], &sender_hash[i]);
                }

                // === ABI event id constraint ===
                // body[2..6) must equal 0x3c838959.
                for (i, &expected_byte) in ABI_EVENT_ID.iter().enumerate() {
                    let expected =
                        ctx.load_constant(Fr::from(expected_byte as u64));
                    ctx.constrain_equal(&body_bytes[EVENT_ABI_PREFIX_START + i], &expected);
                }

                // === Field extraction from body / recipient (cell_repr_data) ===
                // All four are PUBLIC in v2. Each is BE-packed (matching the
                // ABI v2 wire layout); ETH-side reassembly is a straight
                // `bytes -> uint{N}` decode.
                let be_powers = |len: usize| -> Vec<QuantumCell<Fr>> {
                    (0..len)
                        .map(|i| {
                            QuantumCell::Constant(
                                Fr::from(256u64).pow([(len - 1 - i) as u64]),
                            )
                        })
                        .collect()
                };
                let to_existing = |slice: &[AssignedValue<Fr>]| -> Vec<QuantumCell<Fr>> {
                    slice.iter().map(|b| QuantumCell::Existing(*b)).collect()
                };

                // dstChainId (uint256 BE, slot PUB_DST_CHAIN_ID)
                let dst_cells =
                    to_existing(&body_bytes[EVENT_DST_CHAIN_ID_START..EVENT_DST_CHAIN_ID_END]);
                let dst_powers = be_powers(EVENT_DST_CHAIN_ID_FIELD_LEN);
                let dst_chain_id_fr = gate.inner_product(ctx, dst_cells, dst_powers);

                // amount (uint128 BE, slot PUB_AMOUNT)
                let amount_cells = to_existing(&body_bytes[EVENT_AMOUNT_START..EVENT_AMOUNT_END]);
                let amount_powers = be_powers(EVENT_AMOUNT_FIELD_LEN);
                let amount_fr = gate.inner_product(ctx, amount_cells, amount_powers);

                // tokenId (uint32 BE, slot PUB_TOKEN_ID)
                let token_cells =
                    to_existing(&body_bytes[EVENT_TOKEN_ID_START..EVENT_TOKEN_ID_END]);
                let token_powers = be_powers(EVENT_TOKEN_ID_FIELD_LEN);
                let token_id = gate.inner_product(ctx, token_cells, token_powers);

                // recipientHi / recipientLo — split α (10 / 10 BE bytes
                // out of the 20-byte address payload at recipient[2..22)).
                // ETH-side reassembly:
                //   address(uint160((uint256(hi) << 80) | uint256(lo)))
                // TODO(circuit4-v2): see RECIPIENT_HI_*/LO_* doc-comment for
                // alternative splits β (uint128+uint32) and γ (uint80+uint80,
                // numerically identical to α).
                let half_powers_hi = be_powers(RECIPIENT_HALF_LEN);
                let half_powers_lo = be_powers(RECIPIENT_HALF_LEN);
                let recipient_hi_cells =
                    to_existing(&recipient_bytes[RECIPIENT_HI_START..RECIPIENT_HI_END]);
                let recipient_hi_fr = gate.inner_product(ctx, recipient_hi_cells, half_powers_hi);
                let recipient_lo_cells =
                    to_existing(&recipient_bytes[RECIPIENT_LO_START..RECIPIENT_LO_END]);
                let recipient_lo_fr = gate.inner_product(ctx, recipient_lo_cells, half_powers_lo);

                // === d1 refs_count checks (one per cell) ===
                // wrapper=1, body=2, recipient=0, sender=0
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

                constrain_refs_count(
                    ctx,
                    &wrapper_bytes[0],
                    self.entries[0].cell_repr_data[0],
                    1,
                );
                constrain_refs_count(
                    ctx,
                    &body_bytes[0],
                    self.entries[1].cell_repr_data[0],
                    2,
                );
                constrain_refs_count(
                    ctx,
                    &recipient_bytes[0],
                    self.entries[2].cell_repr_data[0],
                    0,
                );
                constrain_refs_count(
                    ctx,
                    &sender_bytes[0],
                    self.entries[3].cell_repr_data[0],
                    0,
                );

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

                // === ext_msg_leaf → ext_out_messages_root (padded) ===
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
                let max_ev_const =
                    ctx.load_constant(Fr::from(MAX_EVENTS_TREE_DEPTH as u64));
                let ev_diff = gate.sub(ctx, max_ev_const, num_events_levels);
                range.range_check(ctx, ev_diff, 4);

                // === events_pos binding ===================================
                // `events_pos` feeds the nullifier Poseidon preimage so that
                // two identical `WithdrawalInitiated` events in one AN block
                // produce distinct nullifiers. That defense is only sound if
                // the value hashed IS the position walked in the events
                // tree — otherwise a malicious prover picks any events_pos
                // to disambiguate the hash and walks a different path.
                //
                // `walk_dense_merkle_bind_pos` bundles the range check,
                // bit-decomposition, zero-forcing loop (bits above
                // `num_events_levels` pinned to 0 to match
                // `preprocess_dense_proof_padded`'s `direction_bit = false`
                // convention), and the walker call. The `events_pos_fr`
                // cell we pass here is the same cell fed into the
                // nullifier Poseidon below — that shared cell is what
                // makes the binding hold.
                let events_pos_fr =
                    ctx.load_witness(Fr::from(self.merkle_proof_position as u64));
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

                // === block_leaf = Poseidon96(block_id, envelope_hash, ext_out_root) ===
                let block_id_fr = ctx.load_witness(bytes_to_fr(&self.block_id));
                let envelope_hash_fr =
                    ctx.load_witness(bytes_to_fr(&self.envelope_hash_bytes));
                let ext_out_root_bytes = if self.merkle_proof_siblings.is_empty() {
                    ext_msg_leaf_native
                } else {
                    fr_to_bytes(compute_root_native(&events_proof_native))
                };
                let block_leaf_fr = poseidon_hash_96_circuit(
                    ctx,
                    &range,
                    &hasher,
                    block_id_fr,
                    envelope_hash_fr,
                    ext_out_root,
                    &self.block_id,
                    &self.envelope_hash_bytes,
                    &ext_out_root_bytes,
                );

                // === block_leaf → history window root (root_1) ===
                let block_leaf_native = poseidon_hash_96_native(
                    &self.block_id,
                    &self.envelope_hash_bytes,
                    &ext_out_root_bytes,
                );
                let block_proof = preprocess_dense_proof(
                    block_leaf_native,
                    &self.block_merkle_proof_siblings,
                    self.block_merkle_proof_position,
                );
                let root_1 = dense_merkle_root_circuit(
                    ctx,
                    &range,
                    &hasher,
                    &block_proof,
                    block_leaf_fr,
                );

                // === root_1 → final_root via dense chain ===
                let num_active =
                    ctx.load_witness(Fr::from(self.num_active_chain_steps as u64));
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

                // === Anchor root =========================================
                // The bridge has no anonymization goal — the verifier
                // simply checks `final_root` against its own state of
                // known layer hashes off-circuit, so `final_root` is
                // exposed as a single public input rather than hidden
                // inside a candidate vector via a private index. The
                // already-computed `final_root` from the dense chain is
                // pushed directly to the instance column below.

                // === senderAccFr (slot PUB_SENDER_ACC_FR) ================
                //
                // Algebraic decode of the sender's 256-bit `account_id` from
                // `entries[3].cell_repr_data` bits [11..267).
                //
                // Sender cell payload layout (34 bytes at sender_bytes[2..36],
                // per EVENT_LAYOUT_COMPARISON.md §2.2):
                //   bits[0..2)   = 0b10        std_addr$10 tag
                //   bits[2..3)   = 0           no anycast
                //   bits[3..11)  = workchain   (0 in fixtures)
                //   bits[11..267)= account_id  (NOT byte-aligned)
                //
                // TVM bit-string convention is MSB-first within each byte,
                // so bit 11 of the payload bit-stream is bit 4 of sender
                // byte 3 (i.e. the low-5-bits of byte 3 form the top 5 bits
                // of `account_id[0]`, joined with the top-3-bits of byte 4).
                //
                // For each sender byte j in [3..36] (33 bytes), split into
                //   sender_bytes[j] = high3[j] * 32 + low5[j]
                // with `range_check(high3, 3)` and `range_check(low5, 5)`.
                // The 32 BE-display-order `account_id` bytes are then
                //   account_id_byte[i] = low5[3+i] * 8 + high3[4+i]
                // and `senderAccFr = sum_i account_id_byte[i] * 256^i`
                // matches the native `bytes_to_fr(account_id)` convention
                // (`Fr::from_raw` on 4 LE u64 limbs, see
                // `gosh_dense_balanced_tree::bytes_to_fr`).
                //
                // Routing bits (tag / anycast / workchain) are NOT
                // constrained here — they are deferred to a future
                // tightening pass (EVENT_LAYOUT_COMPARISON.md §5 "constrain
                // d2 and tighten entries[3] bit-prefix").
                //
                // ---- senderDappFr is NOT exposed (see module head) -----
                // The TVM address type (`std_addr$10`) has NO dApp-id field.
                // `dapp_id` lives in `ShardAccount` state metadata, not in
                // the address; binding it would require a separate
                // `ShardAccount`-state Merkle proof. If the destination
                // side needs the sender's dApp-id, the AN bridge contract
                // developer should add a `senderDappId` field to the
                // `WithdrawalInitiated` event so it appears in the body
                // BOC and can be parsed/bound just like `tokenId`.
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
                // sender_low5[k] corresponds to sender_bytes[3 + k].
                // account_id_byte[i] uses low5 of sender_bytes[3+i] (k = i)
                // and high3 of sender_bytes[4+i] (k = i + 1).
                for i in 0..32 {
                    let combined =
                        gate.mul_add(ctx, sender_low5[i], c8, sender_high3[i + 1]);
                    acc_id_bytes.push(QuantumCell::Existing(combined));
                }
                let le_powers_32_acc: Vec<QuantumCell<Fr>> = (0..32)
                    .map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64])))
                    .collect();
                let sender_acc_fr = gate.inner_product(ctx, acc_id_bytes, le_powers_32_acc);

                // === Nullifier ============================================
                //   Poseidon(block_id_fr, tokenId, amount, recipientHi,
                //            recipientLo, senderAccFr, eventsPos)
                // `eventsPos` makes two identical WithdrawalInitiated
                // events in the same AN block collide-free at the
                // replay-protection layer. It stays PRIVATE — soundness
                // comes from binding its bit-decomposition to the
                // events-tree walker above (`walk_dense_merkle_bind_pos`).
                let nullifier_fr = hasher.hash_fix_len_array(
                    ctx,
                    gate,
                    &[
                        block_id_fr,
                        token_id,
                        amount_fr,
                        recipient_hi_fr,
                        recipient_lo_fr,
                        sender_acc_fr,
                        events_pos_fr,
                    ],
                );

                // === anchorLayer (slot PUB_ANCHOR_LAYER) =================
                // 1-indexed layer number of the anchor `final_root`, bound
                // by the L1 verifier's per-layer HISTORY_PROOF window scan
                // (`_isKnownLayerAnchor` in AckiNackiBridge.sol). Range
                // `1..=MAX_ANCHOR_LAYER` enforced by two 4-bit range checks
                // on `anchor_layer - 1` and `MAX_ANCHOR_LAYER - anchor_layer`
                // (both must be non-negative). Solidity ALSO validates the
                // `anchorLayer` bound on the `withdrawByProof` path — this
                // in-circuit check is defense in depth.
                let anchor_layer_fr =
                    ctx.load_witness(Fr::from(self.anchor_layer as u64));
                let one_const = ctx.load_constant(Fr::one());
                let al_minus_1 = gate.sub(
                    ctx,
                    QuantumCell::Existing(anchor_layer_fr),
                    QuantumCell::Existing(one_const),
                );
                range.range_check(ctx, al_minus_1, 4);
                let max_al_const =
                    ctx.load_constant(Fr::from(MAX_ANCHOR_LAYER as u64));
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
                )
            };

            // Public instances (see module head + `PUB_*` constants):
            //   0  PUB_TOKEN_ID
            //   1  PUB_AMOUNT
            //   2  PUB_RECIPIENT_HI
            //   3  PUB_RECIPIENT_LO
            //   4  PUB_DST_CHAIN_ID
            //   5  PUB_SENDER_ACC_FR
            //   6  PUB_DAPP_FR
            //   7  PUB_ACC_FR
            //   8  PUB_NULLIFIER
            //   9  PUB_FINAL_ROOT
            //  10  PUB_ANCHOR_LAYER
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
    use super::*;
    use crate::event_data_helper::read_withdrawals_from_file;
    use crate::test_helpers::*;
    use dense_balanced_tree::PoseidonHasher as DensePoseidonHasher;
    use halo2_base::halo2_proofs::dev::MockProver;

    // `make_instances` is imported from `test_helpers::*` above — moved
    // there so that downstream crates (e.g.
    // `bridge-prover-lib::keys::ensure_event_keys`) can reuse the exact
    // same synthetic-witness wiring used by these tests.

    /// MockProver pass over every real withdrawal in `withdrawals.txt`.
    #[test]
    fn test_bridge_event_prove_circuit_for_all_collected_events_mock_prover() {
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let recs = read_withdrawals_from_file("withdrawals.txt");
        assert!(
            !recs.is_empty(),
            "withdrawals.txt must contain at least one entry"
        );

        let params = base_circuit_params();
        let all_withdrawals: Vec<WithdrawalFields> =
            recs.iter().map(extract_withdrawal_fields).collect();
        println!("Parsed {} withdrawals", all_withdrawals.len());

        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(42);

        for (idx, w) in all_withdrawals.iter().enumerate() {
            println!("\n========== Withdrawal {} ==========", idx);

            let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
            let (dense_chain, final_root_bytes) =
                build_dense_chain(tw.blocks_root_level_0, 1, 130);
            let final_root_fr = bytes_to_fr(&final_root_bytes);

            let circuit = BridgeEventProveCircuit::new(
                w.entries.clone(),
                tw.events_siblings,
                tw.events_pos,
                tw.account_dapp_id,
                tw.account_id,
                tw.block_id,
                tw.envelope_hash_bytes,
                tw.block_siblings,
                tw.block_pos,
                dense_chain,
                1,
                1,
                params.clone(),
            );

            let leading = compute_leading_public_inputs(
                w,
                &tw.block_id,
                &tw.account_dapp_id,
                &tw.account_id,
                &w.sender_account_id,
                tw.events_pos,
            );
            let instances = make_instances(leading, final_root_fr, Fr::from(1u64));
            let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
            prover.assert_satisfied();
            println!("Withdrawal {} passed", idx);
        }
        println!(
            "\nAll {} withdrawals passed with two-level tree proofs",
            all_withdrawals.len()
        );
    }

    /// Two identical `WithdrawalInitiated` events at different `events_pos`
    /// in the *same* AN block must produce distinct nullifiers. Without the
    /// `events_pos` field in the nullifier preimage, both proofs would hit
    /// the same 6-arg Poseidon preimage and their nullifiers would collide —
    /// the second on-chain payout would be permanently blocked by
    /// `NullifierAlreadyUsed` on the first's slot.
    ///
    /// We reuse the first captured withdrawal and place its `ext_msg_leaf` at
    /// slots 3 and 7 of a single events tree that also shares its
    /// `block_id / account_dapp_id / account_id / envelope_hash` between the
    /// two runs. Two MockProver runs — one per slot — must both
    /// `assert_satisfied()` AND yield different `PUB_NULLIFIER` values.
    #[test]
    fn test_nullifier_distinct_for_same_block_different_events_pos() {
        use dense_balanced_tree::{dense_merkle_proof, dense_merkle_root};
        use rand::rngs::StdRng;
        use rand::SeedableRng;
        use rand::Rng;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260922);

        // Same-block scenario: dapp / account / block_id / envelope are shared
        // between the two proofs — the ONLY witness difference is which slot
        // of the events tree we walk to.
        let mut dapp_id = [0u8; 32];
        let mut account_id_b = [0u8; 32];
        let mut block_id = [0u8; 32];
        let mut envelope_hash = [0u8; 32];
        rng.fill(&mut dapp_id);
        rng.fill(&mut account_id_b);
        rng.fill(&mut block_id);
        rng.fill(&mut envelope_hash);

        const NUM_EVENTS_LEAVES: usize = 128;
        const NUM_BLOCK_LEAVES: usize = 130;
        const POS_A: usize = 3;
        const POS_B: usize = 7;

        let ext_msg_leaf =
            poseidon_hash_96_native(&dapp_id, &account_id_b, &w.repr_hash);

        // Same ext_msg_leaf at TWO slots — this is precisely the collision
        // that would have been masked at the nullifier level without the
        // `events_pos` binding.
        let mut events_leaves = vec![[0u8; 32]; NUM_EVENTS_LEAVES];
        for leaf in events_leaves.iter_mut() {
            rng.fill(leaf);
        }
        events_leaves[POS_A] = ext_msg_leaf;
        events_leaves[POS_B] = ext_msg_leaf;

        let events_root = dense_merkle_root(&dense_hasher, &events_leaves);
        let siblings_a = dense_merkle_proof(&dense_hasher, &events_leaves, POS_A);
        let siblings_b = dense_merkle_proof(&dense_hasher, &events_leaves, POS_B);

        // Single-block tree — same block_leaf, so the block-tree proof and
        // the downstream dense chain are identical between the two runs.
        let block_leaf =
            poseidon_hash_96_native(&block_id, &envelope_hash, &events_root);
        let mut block_leaves = vec![[0u8; 32]; NUM_BLOCK_LEAVES];
        for leaf in block_leaves.iter_mut() {
            rng.fill(leaf);
        }
        block_leaves[0] = block_leaf;
        let blocks_root = dense_merkle_root(&dense_hasher, &block_leaves);
        let block_siblings = dense_merkle_proof(&dense_hasher, &block_leaves, 0);

        let (dense_chain, final_root_bytes) = build_dense_chain(blocks_root, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let mut nullifiers = Vec::new();
        for (events_pos, events_siblings) in [
            (POS_A, siblings_a.clone()),
            (POS_B, siblings_b.clone()),
        ] {
            let circuit = BridgeEventProveCircuit::new(
                w.entries.clone(),
                events_siblings,
                events_pos,
                dapp_id,
                account_id_b,
                block_id,
                envelope_hash,
                block_siblings.clone(),
                0,
                dense_chain.clone(),
                1,
                1, // anchor_layer — legal, keeps range-check happy
                params.clone(),
            );
            let leading = compute_leading_public_inputs(
                &w,
                &block_id,
                &dapp_id,
                &account_id_b,
                &w.sender_account_id,
                events_pos,
            );
            let instances = make_instances(leading, final_root_fr, Fr::from(1u64));
            let prover =
                MockProver::<Fr>::run(K, &circuit, vec![instances.clone()]).unwrap();
            prover.assert_satisfied();
            nullifiers.push(instances[PUB_NULLIFIER]);
        }

        assert_ne!(
            nullifiers[0], nullifiers[1],
            "two identical WithdrawalInitiated events at different \
             events_pos in the same AN block must yield distinct nullifiers"
        );
    }

    /// The in-circuit range check on `anchor_layer`
    /// (two 4-bit lookups on `anchor_layer - 1` and `MAX_ANCHOR_LAYER
    /// - anchor_layer`) must satisfy exactly the closed interval
    /// `1..=MAX_ANCHOR_LAYER`. The on-chain verifier does not trust the
    /// circuit alone here: `AckiNackiBridge.withdrawByProof` re-checks the
    /// same range (`pub.anchorLayer == 0 || pub.anchorLayer >
    /// MAX_LAYER_HASHES → InvalidNumLayers`) before routing to
    /// `layerWindows[anchorLayer]`. This test's job is to pin the circuit
    /// side so the two range checks agree.
    ///
    /// Positive sweep: `{1, 5, MAX_ANCHOR_LAYER}` all `assert_satisfied()`.
    ///
    /// Negative sweep: `{0, MAX_ANCHOR_LAYER + 1}` — the constructor's own
    /// `assert_invariants` refuses these on any legitimate call site, so
    /// the negative branch instantiates `BridgeEventProveCircuit` directly
    /// (all fields are `pub`) to actually exercise the in-circuit range
    /// check rather than the Rust-side guard. `MockProver::verify` must
    /// return `Err`.
    #[test]
    fn test_anchor_layer_range_boundary() {
        use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
        use rand::rngs::StdRng;
        use rand::SeedableRng;
        use std::cell::RefCell;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260922_2);

        // Shared witnesses — only `anchor_layer` (witness + PI slot 10)
        // varies across the sweep.
        let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
        let (dense_chain, final_root_bytes) =
            build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let leading = compute_leading_public_inputs(
            &w,
            &tw.block_id,
            &tw.account_dapp_id,
            &tw.account_id,
            &w.sender_account_id,
            tw.events_pos,
        );

        // Positive: legal values must all satisfy.
        for anchor_layer in [1u8, 5u8, MAX_ANCHOR_LAYER] {
            let circuit = BridgeEventProveCircuit::new(
                w.entries.clone(),
                tw.events_siblings.clone(),
                tw.events_pos,
                tw.account_dapp_id,
                tw.account_id,
                tw.block_id,
                tw.envelope_hash_bytes,
                tw.block_siblings.clone(),
                tw.block_pos,
                dense_chain.clone(),
                1,
                anchor_layer,
                params.clone(),
            );
            let instances = make_instances(
                leading,
                final_root_fr,
                Fr::from(anchor_layer as u64),
            );
            let prover =
                MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
            prover.assert_satisfied();
            println!("anchor_layer={} accepted", anchor_layer);
        }

        // Negative: 0 and `MAX_ANCHOR_LAYER + 1` must be rejected by the
        // in-circuit range check. `BridgeEventProveCircuit::new` refuses
        // both on the Rust side, so we build the struct directly (all
        // fields are `pub`) to isolate the ZK constraint under test.
        for bad_layer in [0u8, MAX_ANCHOR_LAYER + 1] {
            let circuit = BridgeEventProveCircuit {
                entries: w.entries.clone(),
                merkle_proof_siblings: tw.events_siblings.clone(),
                merkle_proof_position: tw.events_pos,
                account_dapp_id: tw.account_dapp_id,
                account_id: tw.account_id,
                block_id: tw.block_id,
                envelope_hash_bytes: tw.envelope_hash_bytes,
                block_merkle_proof_siblings: tw.block_siblings.clone(),
                block_merkle_proof_position: tw.block_pos,
                dense_chain: dense_chain.clone(),
                num_active_chain_steps: 1,
                anchor_layer: bad_layer,
                base_circuit_params: params.clone(),
                base_circuit_builder: RefCell::new(
                    BaseCircuitBuilder::<Fr>::new(false)
                        .use_params(params.clone()),
                ),
            };
            let instances = make_instances(
                leading,
                final_root_fr,
                Fr::from(bad_layer as u64),
            );
            let prover =
                MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
            let verdict = prover.verify();
            assert!(
                verdict.is_err(),
                "anchor_layer={} (outside 1..={}) must be rejected by the \
                 in-circuit range check, but MockProver::verify returned Ok",
                bad_layer,
                MAX_ANCHOR_LAYER,
            );
            println!("anchor_layer={} rejected as expected", bad_layer);
        }
    }

    /// Pin the PI/witness equality on slot 10: build a valid circuit at
    /// `anchor_layer = 2`, then assemble the instance vector with slot 10
    /// = `Fr::from(3)`. `MockProver::verify` must return `Err`. Not an
    /// anchor-specific soundness property — the same shape holds for
    /// every PI slot — but slot 10 is the value the contract routes on.
    #[test]
    fn test_anchor_layer_pi_witness_mismatch() {
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260922_3);

        let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
        let (dense_chain, final_root_bytes) =
            build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let witness_layer: u8 = 2;
        let pi_layer: u8 = 3;

        let circuit = BridgeEventProveCircuit::new(
            w.entries.clone(),
            tw.events_siblings.clone(),
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id,
            tw.envelope_hash_bytes,
            tw.block_siblings.clone(),
            tw.block_pos,
            dense_chain.clone(),
            1,
            witness_layer,
            params.clone(),
        );

        let leading = compute_leading_public_inputs(
            &w,
            &tw.block_id,
            &tw.account_dapp_id,
            &tw.account_id,
            &w.sender_account_id,
            tw.events_pos,
        );
        // Assemble instances with a DIFFERENT anchor_layer at PI slot 10
        // than the witness the circuit was built with.
        let instances = make_instances(leading, final_root_fr, Fr::from(pi_layer as u64));

        let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
        let verdict = prover.verify();
        assert!(
            verdict.is_err(),
            "witness anchor_layer={} but PI slot 10 = {} — the copy \
             constraint must reject, but MockProver::verify returned Ok",
            witness_layer,
            pi_layer,
        );
    }

    /// Canonical-order guard: the on-chain `PUB_NULLIFIER` slot
    /// must equal `poseidon_hash([block_id, token_id, amount, recip_hi,
    /// recip_lo, sender_acc, events_pos])` — in that exact order. A silent
    /// reorder inside `nullifier_native` (the wrapper every witness path
    /// funnels through) would still pass every other regression here because
    /// prover and verifier would agree on the wrong preimage.
    ///
    /// Part 1 (fast): fuzz `nullifier_native` against a direct
    /// `poseidon_hash` call over K random 7-tuples — no circuit, pure native.
    /// Part 2 (one MockProver): assert `instances[PUB_NULLIFIER]` — the slot
    /// wired to the on-chain verifier — equals `poseidon_hash` on the
    /// canonically ordered inputs read straight from the withdrawal fields,
    /// independent of `nullifier_native`.
    #[test]
    fn test_nullifier_canonical_argument_order() {
        use rand::rngs::StdRng;
        use rand::Rng;
        use rand::SeedableRng;

        // Part 1: native fuzz — catches wrapper reorders without MockProver
        // cost, and documents the canonical order via the direct call.
        let mut rng = StdRng::seed_from_u64(20260922_5);
        for _ in 0..16 {
            let mut buf = [0u8; 32];
            let mut fr_of = || {
                rng.fill(&mut buf);
                bytes_to_fr(&buf)
            };
            let block_id = fr_of();
            let token_id = fr_of();
            let amount = fr_of();
            let hi = fr_of();
            let lo = fr_of();
            let sender_acc = fr_of();
            let events_pos = fr_of();

            let expected = poseidon_hash(&[
                block_id, token_id, amount, hi, lo, sender_acc, events_pos,
            ]);
            let via_helper = nullifier_native(
                block_id, token_id, amount, hi, lo, sender_acc, events_pos,
            );
            assert_eq!(
                expected, via_helper,
                "canonical 7-arg nullifier order violated by nullifier_native"
            );
        }

        // Part 2: single MockProver — the PI slot fed to the on-chain
        // verifier must match `poseidon_hash` on canonical inputs.
        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(20260922_6);
        let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
        let (dense_chain, final_root_bytes) =
            build_dense_chain(tw.blocks_root_level_0, 1, 130);
        let final_root_fr = bytes_to_fr(&final_root_bytes);
        let params = base_circuit_params();

        let circuit = BridgeEventProveCircuit::new(
            w.entries.clone(),
            tw.events_siblings.clone(),
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id,
            tw.envelope_hash_bytes,
            tw.block_siblings.clone(),
            tw.block_pos,
            dense_chain.clone(),
            1,
            1,
            params.clone(),
        );

        let leading = compute_leading_public_inputs(
            &w,
            &tw.block_id,
            &tw.account_dapp_id,
            &tw.account_id,
            &w.sender_account_id,
            tw.events_pos,
        );
        let instances = make_instances(leading, final_root_fr, Fr::from(1u64));
        let prover =
            MockProver::<Fr>::run(K, &circuit, vec![instances.clone()]).unwrap();
        prover.assert_satisfied();

        let expected_pub_nullifier = poseidon_hash(&[
            bytes_to_fr(&tw.block_id),
            w.token_id_val,
            w.amount_val,
            w.recipient_hi_val,
            w.recipient_lo_val,
            bytes_to_fr(&w.sender_account_id),
            Fr::from(tw.events_pos as u64),
        ]);
        assert_eq!(
            instances[PUB_NULLIFIER], expected_pub_nullifier,
            "PUB_NULLIFIER slot must equal poseidon_hash on canonically \
             ordered [block_id, token_id, amount, recip_hi, recip_lo, \
             sender_acc, events_pos]"
        );
    }

    /// Exercise every supported dense-chain length T = 0..=MAX_CHAIN_LEN with
    /// the first real withdrawal.
    #[test]
    fn test_bridge_event_prove_circuit_merkle_chain_variable_length() {
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(77);
        let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);

        let params = base_circuit_params();

        for t in 0..=MAX_CHAIN_LEN {
            println!("\n========== Chain T={} ==========", t);

            let (dense_chain, final_root_bytes) =
                build_dense_chain(tw.blocks_root_level_0, t, 130);
            let final_root_fr = bytes_to_fr(&final_root_bytes);

            let circuit = BridgeEventProveCircuit::new(
                w.entries.clone(),
                tw.events_siblings.clone(),
                tw.events_pos,
                tw.account_dapp_id,
                tw.account_id,
                tw.block_id,
                tw.envelope_hash_bytes,
                tw.block_siblings.clone(),
                tw.block_pos,
                dense_chain,
                t,
                1,
                params.clone(),
            );

            let leading = compute_leading_public_inputs(
                &w,
                &tw.block_id,
                &tw.account_dapp_id,
                &tw.account_id,
                &w.sender_account_id,
                tw.events_pos,
            );
            let instances = make_instances(leading, final_root_fr, Fr::from(1u64));
            let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
            prover.assert_satisfied();
            println!("T={} passed!", t);
        }
        println!("\nAll chain lengths T=0..{} passed!", MAX_CHAIN_LEN);
    }

    /// Real (non-mock) prover: keygen once at T=1, then prove for a sweep of
    /// chain lengths and verify each proof.
    ///
    /// Marked `#[ignore]` because it runs a real halo2 keygen at `K = 19`,
    /// which is orders of magnitude heavier than MockProver and too slow
    /// for a per-MR CI job. Wall-clock and PK size depend on chip params;
    /// they are not the same as the production wrapper's numbers (which
    /// runs against a `K = 20` SRS with a correspondingly larger PK). Run
    /// on demand with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_bridge_event_prove_circuit_real_proof_for_fixed_k() {
        use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk};
        use halo2_base::utils::fs::gen_srs;
        use halo2_base::utils::testing::{check_proof_with_instances, gen_proof_with_instances};
        use rand::rngs::StdRng;
        use rand::SeedableRng;
        use std::time::Instant;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let mut rng = StdRng::seed_from_u64(99);
        let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);

        let params = base_circuit_params();
        let srs = gen_srs(K);

        // Keygen once at T=1; verify_chain_of_dense_proofs always processes
        // MAX_CHAIN_LEN links so circuit shape does not change with chain_len.
        let (keygen_chain, _keygen_final_root_bytes) =
            build_dense_chain(tw.blocks_root_level_0, 1, 130);

        let keygen_circuit = BridgeEventProveCircuit::new(
            w.entries.clone(),
            tw.events_siblings.clone(),
            tw.events_pos,
            tw.account_dapp_id,
            tw.account_id,
            tw.block_id,
            tw.envelope_hash_bytes,
            tw.block_siblings.clone(),
            tw.block_pos,
            keygen_chain,
            1,
            1,
            params.clone(),
        );

        let start = Instant::now();
        let vk = keygen_vk(&srs, &keygen_circuit).expect("keygen_vk should not fail");
        println!("keygen_vk time: {:?}", start.elapsed());

        let start = Instant::now();
        let pk = keygen_pk(&srs, vk, &keygen_circuit).expect("keygen_pk should not fail");
        println!("keygen_pk time: {:?}", start.elapsed());

        let break_points = keygen_circuit.base_circuit_builder.borrow().break_points();

        let chain_lengths = [0, 1, 2, 5, MAX_CHAIN_LEN];

        struct ProofResult {
            chain_len: usize,
            prove_ms: u128,
            verify_ms: u128,
            proof_size: usize,
        }
        let mut results: Vec<ProofResult> = Vec::new();

        for &chain_len in &chain_lengths {
            println!("\n========== Real proof: chain_len={} ==========", chain_len);

            let (dense_chain, final_root_bytes) =
                build_dense_chain(tw.blocks_root_level_0, chain_len, 130);
            let final_root_fr = bytes_to_fr(&final_root_bytes);

            let prover_circuit = BridgeEventProveCircuit::new_for_proving(
                w.entries.clone(),
                tw.events_siblings.clone(),
                tw.events_pos,
                tw.account_dapp_id,
                tw.account_id,
                tw.block_id,
                tw.envelope_hash_bytes,
                tw.block_siblings.clone(),
                tw.block_pos,
                dense_chain,
                chain_len,
                1,
                params.clone(),
                break_points.clone(),
            );

            let start = Instant::now();
            let leading = compute_leading_public_inputs(
                &w,
                &tw.block_id,
                &tw.account_dapp_id,
                &tw.account_id,
                &w.sender_account_id,
                tw.events_pos,
            );
            let instance_fr = make_instances(leading, final_root_fr, Fr::from(1u64));
            let proof_bytes =
                gen_proof_with_instances(&srs, &pk, prover_circuit, &[&instance_fr]);
            let prove_ms = start.elapsed().as_millis();
            println!("  proof generation time: {}ms", prove_ms);
            println!("  proof size: {} bytes", proof_bytes.len());

            let start = Instant::now();
            check_proof_with_instances(&srs, pk.get_vk(), &proof_bytes, &[&instance_fr], true);
            let verify_ms = start.elapsed().as_millis();
            println!("  proof verification time: {}ms", verify_ms);
            println!("  chain_len={} passed!", chain_len);

            results.push(ProofResult {
                chain_len,
                prove_ms,
                verify_ms,
                proof_size: proof_bytes.len(),
            });
        }

        println!("\n╔════════════╤═══════════╤═══════════╤════════════╗");
        println!("║ chain_len  │  prove    │  verify   │ proof_size ║");
        println!("╠════════════╪═══════════╪═══════════╪════════════╣");
        for r in &results {
            println!(
                "║    {:>2}      │  {:>6}ms │  {:>6}ms │  {:>6}B   ║",
                r.chain_len, r.prove_ms, r.verify_ms, r.proof_size,
            );
        }
        println!("╚════════════╧═══════════╧═══════════╧════════════╝");
    }

    /// Sanity-check that the circuit accepts events-tree proofs of every
    /// depth in `[0, MAX_EVENTS_TREE_DEPTH]`, using the first real
    /// withdrawal as the leaf at position 0.
    #[test]
    fn test_bridge_event_prove_circuit_variable_events_depth() {
        use rand::rngs::StdRng;
        use rand::SeedableRng;

        let w = load_first_withdrawal();
        let dense_hasher = DensePoseidonHasher::new();
        let params = base_circuit_params();

        for depth in 0..=MAX_EVENTS_TREE_DEPTH {
            let num_events_leaves = 1usize << depth;
            println!(
                "\n========== Events depth = {} ({} leaves) ==========",
                depth, num_events_leaves
            );

            let mut rng = StdRng::seed_from_u64(101 + depth as u64);
            let tw = build_two_level_tree(
                &w.repr_hash,
                &mut rng,
                &dense_hasher,
                num_events_leaves,
                130,
            );
            assert_eq!(tw.events_siblings.len(), depth);

            let (dense_chain, final_root_bytes) =
                build_dense_chain(tw.blocks_root_level_0, 1, 130);
            let final_root_fr = bytes_to_fr(&final_root_bytes);

            let events_pos = tw.events_pos;
            let circuit = BridgeEventProveCircuit::new(
                w.entries.clone(),
                tw.events_siblings,
                tw.events_pos,
                tw.account_dapp_id,
                tw.account_id,
                tw.block_id,
                tw.envelope_hash_bytes,
                tw.block_siblings,
                tw.block_pos,
                dense_chain,
                1,
                1,
                params.clone(),
            );

            let leading = compute_leading_public_inputs(
                &w,
                &tw.block_id,
                &tw.account_dapp_id,
                &tw.account_id,
                &w.sender_account_id,
                events_pos,
            );
            let instances = make_instances(leading, final_root_fr, Fr::from(1u64));
            let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
            prover.assert_satisfied();
            println!("depth={} passed!", depth);
        }
    }

    /// Smoke-test the one-shot `build_synthetic_event_keygen_inputs` helper
    /// used by downstream crates (e.g. `bridge-prover-lib::keys`) for keygen.
    /// Just exercises MockProver to confirm the wiring is constraint-clean.
    ///
    /// Coverage caveat: the synthetic witness this helper builds uses
    /// `events_pos = 0` (see `test_helpers::build_synthetic_withdrawal`), so
    /// a regression that silently replaces the in-circuit nullifier's
    /// `events_pos` term with a literal `0` would still pass here. That
    /// specific bug is caught by
    /// `test_nullifier_distinct_for_same_block_different_events_pos`, which
    /// runs two proofs against the same block with distinct positions and
    /// asserts the two `PUB_NULLIFIER` slots differ.
    #[test]
    fn test_build_synthetic_event_keygen_inputs_mock_prover() {
        let (circuit, instances) = build_synthetic_event_keygen_inputs(0xC0FFEE);
        assert_eq!(instances.len(), TOTAL_PUBLIC_INPUTS);
        let prover = MockProver::<Fr>::run(K, &circuit, vec![instances]).unwrap();
        prover.assert_satisfied();
    }

    /// Sanity check on the native nullifier slot produced by
    /// `compute_leading_public_inputs` (= `nullifier_native(...)`): the value
    /// at `PUB_NULLIFIER` must be non-zero (Fr::zero would mean the hasher
    /// silently absorbed nothing).
    ///
    /// This test does NOT run MockProver and therefore does not itself pin
    /// circuit-vs-native equality on the nullifier slot. That equality is
    /// covered by `test_build_synthetic_event_keygen_inputs_mock_prover`
    /// (which does invoke MockProver and would surface any instance-slot
    /// drift, including on `PUB_NULLIFIER`).
    #[test]
    fn test_nullifier_recomputes_natively() {
        let (_circuit, instances) = build_synthetic_event_keygen_inputs(0xDEAD_BEEF);
        let nullifier = instances[PUB_NULLIFIER];
        assert_ne!(nullifier, Fr::zero(), "nullifier must not be zero");
    }

    /// Verify recipient α-split: reading the two halves back and
    /// reassembling them BE-style yields the 20-byte address.
    #[test]
    fn test_recipient_split_roundtrip() {
        let w = load_first_withdrawal();
        let parsed_recipient: [u8; RECIPIENT_LEN_FIXED] = {
            let mut a = [0u8; RECIPIENT_LEN_FIXED];
            a.copy_from_slice(&w.entries[2].cell_repr_data[2..2 + RECIPIENT_LEN_FIXED]);
            a
        };
        let hi_native = be_bytes_to_fr(&parsed_recipient[..RECIPIENT_HALF_LEN]);
        let lo_native = be_bytes_to_fr(&parsed_recipient[RECIPIENT_HALF_LEN..]);
        assert_eq!(hi_native, w.recipient_hi_val);
        assert_eq!(lo_native, w.recipient_lo_val);
    }
}
