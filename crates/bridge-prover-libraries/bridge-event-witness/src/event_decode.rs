//! `WithdrawalInitiated` ExtOut message decoding.
//!
//! Mirrors the structural invariants enforced by
//! `bridge-event-prove-circuit::test_helpers::parse_withdrawal_boc`:
//!   entries[0] = ExtOut wrapper, refs=1
//!   entries[1] = body cell,      refs=2, fixed 146 bytes, ABI id 0x728804fc
//!   entries[2] = recipient cell, refs=0, fixed 22 bytes  (d1+d2 + 20-byte addr)
//!   entries[3] = sender cell,    refs=0, fixed 36 bytes  (std_addr$10 + 256-bit acc_id)
//!
//! Body byte layout (cell_repr_data-relative):
//!   [0..2)     d1 + d2
//!   [2..6)     ABI event id = 0x728804fc
//!   [6..38)    dstChainId (uint256 BE)
//!   [38..54)   amount     (uint128 BE)
//!   [54..58)   tokenId    (uint32  BE)
//!   [58..78)   dstBridge  (uint160 BE, the EVM bridge the withdrawal is bound to)
//!   [78..82)   child_depths
//!   [82..114)  sha256(recipient cell)
//!   [114..146) sha256(sender cell)
//!
//! These constants are pinned in
//! `bridge-event-prove-circuit::bridge_event_prove_circuit`; keep in sync if
//! that crate's layout ever changes.

use anyhow::{anyhow, bail, Result};

use crate::boc_walk::FlatCell;
use crate::schema::WithdrawalInitiated;

/// ABI event id of `WithdrawalInitiated` (first 4 bytes of the truncated
/// SHA-256 of the canonical signature, per TVM Solidity ABI v2).
pub const ABI_EVENT_ID: [u8; 4] = [0x72, 0x88, 0x04, 0xfc];

pub const BODY_CELL_LEN: usize = 146;
pub const RECIPIENT_LEN_FIXED: usize = 20;
pub const RECIPIENT_CELL_LEN: usize = 2 + RECIPIENT_LEN_FIXED;
pub const SENDER_CELL_LEN: usize = 2 + 34;

const EVENT_ABI_PREFIX_START: usize = 2;
const EVENT_ABI_PREFIX_END: usize = 6;
const EVENT_DST_CHAIN_ID_START: usize = 6;
const EVENT_DST_CHAIN_ID_END: usize = 38;
const EVENT_AMOUNT_START: usize = 38;
const EVENT_AMOUNT_END: usize = 54;
const EVENT_TOKEN_ID_START: usize = 54;
const EVENT_TOKEN_ID_END: usize = 58;
const EVENT_DST_BRIDGE_START: usize = 58;
const EVENT_DST_BRIDGE_END: usize = 78;

/// Validate the 4-cell BFS layout produced by `serialize_cells_tree_root_first`
/// against the structural invariants the circuit depends on.
///
/// Returns the entries as a `[FlatCell; 4]` if valid.
pub fn validate_layout(cells: Vec<FlatCell>) -> Result<[FlatCell; 4]> {
    if cells.len() != 4 {
        bail!(
            "expected 4 cells in WithdrawalInitiated BOC (wrapper, body, recipient, sender); got {}",
            cells.len(),
        );
    }

    if cells[0].refs_count != 1 {
        bail!(
            "entries[0] (ExtOut wrapper) must have refs_count=1, got {}",
            cells[0].refs_count,
        );
    }

    if cells[1].refs_count != 2 {
        bail!(
            "entries[1] (body) must have refs_count=2, got {}",
            cells[1].refs_count,
        );
    }
    if cells[1].cell_repr_data.len() != BODY_CELL_LEN {
        bail!(
            "body cell payload length mismatch (got {}, expected {})",
            cells[1].cell_repr_data.len(),
            BODY_CELL_LEN,
        );
    }

    let abi_slice = &cells[1].cell_repr_data[EVENT_ABI_PREFIX_START..EVENT_ABI_PREFIX_END];
    if abi_slice != ABI_EVENT_ID {
        bail!(
            "ABI event id mismatch at body[2..6) — expected WithdrawalInitiated 0x728804fc, got {:02x?}",
            abi_slice,
        );
    }

    if cells[2].refs_count != 0 {
        bail!(
            "entries[2] (recipient) must have refs_count=0, got {}",
            cells[2].refs_count,
        );
    }
    if cells[2].cell_repr_data.len() != RECIPIENT_CELL_LEN {
        bail!(
            "recipient cell payload length mismatch (got {}, expected {} for 20-byte recipient)",
            cells[2].cell_repr_data.len(),
            RECIPIENT_CELL_LEN,
        );
    }

    if cells[3].refs_count != 0 {
        bail!(
            "entries[3] (sender) must have refs_count=0, got {}",
            cells[3].refs_count,
        );
    }
    if cells[3].cell_repr_data.len() != SENDER_CELL_LEN {
        bail!(
            "sender cell payload length mismatch (got {}, expected {})",
            cells[3].cell_repr_data.len(),
            SENDER_CELL_LEN,
        );
    }

    let mut iter = cells.into_iter();
    Ok([
        iter.next().unwrap(),
        iter.next().unwrap(),
        iter.next().unwrap(),
        iter.next().unwrap(),
    ])
}

/// Extract the `WithdrawalInitiated` fields from a validated 4-entry layout.
pub fn decode_event(entries: &[FlatCell; 4]) -> Result<WithdrawalInitiated> {
    let body = &entries[1].cell_repr_data;
    let recipient_data = &entries[2].cell_repr_data;

    let dst_chain_id_hex = hex::encode(&body[EVENT_DST_CHAIN_ID_START..EVENT_DST_CHAIN_ID_END]);
    let amount_hex = hex::encode(&body[EVENT_AMOUNT_START..EVENT_AMOUNT_END]);
    let token_id_bytes: [u8; 4] = body[EVENT_TOKEN_ID_START..EVENT_TOKEN_ID_END]
        .try_into()
        .map_err(|_| anyhow!("body slice for tokenId is not 4 bytes"))?;
    let token_id = u32::from_be_bytes(token_id_bytes);
    let dst_bridge_hex = hex::encode(&body[EVENT_DST_BRIDGE_START..EVENT_DST_BRIDGE_END]);
    let recipient_hex = hex::encode(&recipient_data[2..2 + RECIPIENT_LEN_FIXED]);
    let sender_hex = hex::encode(&entries[3].cell_repr_data[2..SENDER_CELL_LEN]);

    Ok(WithdrawalInitiated {
        dst_chain_id_hex,
        amount_hex,
        token_id,
        dst_bridge_hex,
        recipient_hex,
        sender_hex,
    })
}
