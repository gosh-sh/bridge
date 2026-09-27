//! Shared byte-layout constants, Poseidon-of-96-bytes helpers and event-body
//! offsets used by both the multi-thread `BridgeEventFinalProof` (Circuit 4
//! final) and the `MultiHopProof` (Circuit 4 multi-hop) paths.
//!
//! Extracted verbatim (minus the legacy `TOTAL_PUBLIC_INPUTS = 11` constant)
//! from the removed single-thread `bridge_event_prove_circuit.rs`. The circuit
//! type that used to live alongside these primitives has been deleted as part
//! of the multi-thread Circuit-4 migration; see the CHANGELOG
//! `## [Unreleased]` `### Breaking Changes` entry and
//! `bridge_event_final_proof.rs` for the current in-circuit consumer.
//!
//! Event body byte layout (cell_repr_data-relative offsets):
//!   [0..2)    d1 + d2
//!   [2..6)    ABI event id  (0x3c838959 for WithdrawalInitiated)
//!   [6..38)   dstChainId    (uint256, 32 B, BE)              ← private
//!   [38..54)  amount        (uint128, 16 B, BE)              ← private
//!   [54..58)  tokenId       (uint32,   4 B, BE)              ← public
//!   [58..62)  child_depths  (2 × u16 BE)
//!   [62..94)  child_hash[0] = sha256(recipient cell)
//!   [94..126) child_hash[1] = sha256(sender cell)
//!
//! Recipient cell `cell_repr_data` layout:
//!   [0..2)   d1 + d2
//!   [2..22)  20 raw recipient address bytes
//!
//! v2 split α (default): hi = BE-pack(bytes[2..12)), lo =
//! BE-pack(bytes[12..22)). Each half holds 80 bits (uint80).

use gosh_dense_balanced_tree::{bytes_to_fr, poseidon_hash_native};
use halo2_base::{
    gates::{GateInstructions, RangeInstructions},
    halo2_proofs::halo2curves::{bn256::Fr, ff::Field as _},
    poseidon::hasher::PoseidonHasher,
    AssignedValue, Context, QuantumCell,
};

use crate::poseidon::*;

pub const MAX_EVENTS_TREE_DEPTH: usize = 8;

/// Maximum 1-indexed layer number for `PUB_ANCHOR_LAYER`. Must equal the
/// Solidity `MAX_LAYER_HASHES` in `AckiNackiBridge.sol` (currently 10). Any
/// change here MUST land together with the on-chain constant — the verifier
/// range-checks `pub.anchorLayer > MAX_LAYER_HASHES` and reverts with
/// `InvalidNumLayers(numLayers)`, so a circuit that emits
/// `anchorLayer > 10` would produce proofs the bridge rejects with that
/// specific selector (not silently).
pub const MAX_ANCHOR_LAYER: u8 = 10;

// ───── Public-input slot indices shared by both circuits ──────────────────
//
// Slots `[0..=10]` mirror the legacy single-thread layout byte-for-byte.
// `BridgeEventFinalProof` extends this with slots `[11..=12]` (`PUB_X_BLOCK_ID`
// / `PUB_Y_BLOCK_ID`) — defined alongside its own `TOTAL_PUBLIC_INPUTS = 13`
// in `bridge_event_final_proof.rs`. `TOTAL_PUBLIC_INPUTS` is therefore NOT
// defined here — each circuit owns its own total.

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

/// First-cut hardcoded recipient length — Ethereum addresses are 20 bytes.
/// All 10 captured fixtures match this. To support variable lengths,
/// see TODO §5.6 in `EVENT_LAYOUT_COMPARISON.md`.
pub const RECIPIENT_LEN_FIXED: usize = 20;

// ───── Event body byte layout (cell_repr_data-relative offsets) ────────────
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
    gosh_dense_balanced_tree::fr_to_bytes(hash)
}

/// In-circuit: Poseidon hash of 3 × 32-byte inputs with algebraic linking.
/// Identical to dark-dex's `poseidon_hash_96_circuit`.
///
/// Public so that the multi-thread `BridgeEventFinalProof` circuit
/// (`bridge_event_final_proof.rs`) can reuse the exact same gadget without
/// duplicating the algebraic-linking arithmetic.
pub fn poseidon_hash_96_circuit(
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
