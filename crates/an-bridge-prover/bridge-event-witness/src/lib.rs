//! Per-transaction private witness data Circuit 4 needs to prove a
//! `WithdrawalInitiated` event from `TokenBridge.sol`.
//!
//! Output schema pinned at `schema_version = 1` in [`schema::SCHEMA_VERSION`].
//!
//! ### Library API
//!
//! [`export_from_event_boc_base64`] — hermetic exporter. Inputs:
//!   * Base64-encoded `Message` BOC of the ExtOut event (matches the format
//!     of `bridge-event-prove-circuit/withdrawals.txt`)
//!   * Block-level context (block_id, account_dapp_id, account_id, envelope
//!     hash) — supplied by the caller; the exporter cannot derive these from
//!     the event BOC alone.
//!
//! Three fields it leaves `None`, populated by the enrichment binary
//! (`bin/build.rs`):
//!   * `events_tree_proof` — Merkle proof from `ext_msg_leaf` to
//!     `ext_out_messages_root`
//!   * `block_tree_proof`  — Merkle proof from `block_leaf` to `root_1`
//!   * `anchor`             — verifier-state-derived layer hash + dense chain
//!
//! ### Binaries
//!
//!   * `bridge-event-private-witness-export` (`bin/export.rs`) — hermetic
//!     CLI wrapper over [`export_from_event_boc_base64`]. No GQL, no state.
//!   * `bridge-event-witness-builder` (`bin/build.rs`) — daemon-side
//!     enrichment: reads a partial `PrivateWitness` JSON, queries GQL +
//!     bridge state, writes an enriched `PrivateWitness` JSON the Halo2
//!     prover can consume.

pub mod boc_walk;
pub mod event_decode;
pub mod schema;

use anyhow::{Context, Result};
use tvm_block::{Deserializable, Message, Serializable};

use crate::boc_walk::{serialize_cells_tree_root_first, FlatCell};
use crate::event_decode::{decode_event, validate_layout};
use crate::schema::{
    BlockContext, CellRecord, PrivateWitness, SCHEMA_VERSION,
};

/// Caller-supplied block context fields. These come from the GraphQL block
/// metadata (or, in test mode, hand-typed CLI flags).
#[derive(Debug, Clone)]
pub struct BlockContextInput {
    pub block_id: [u8; 32],
    pub block_seq_no: u64,
    pub account_dapp_id: [u8; 32],
    pub account_id: [u8; 32],
    pub envelope_hash: [u8; 32],
}

/// Parse a base64-encoded ExtOut `Message` BOC, flatten the 4-cell DAG, decode
/// the `WithdrawalInitiated` payload, and return a `PrivateWitness` with the
/// daemon-side fields (`events_tree_proof`, `block_tree_proof`, `anchor`)
/// left `None`.
pub fn export_from_event_boc_base64(
    event_boc_b64: &str,
    ctx: &BlockContextInput,
) -> Result<PrivateWitness> {
    let msg = Message::construct_from_base64(event_boc_b64)
        .map_err(|e| anyhow::anyhow!("failed to parse event BOC from base64: {e}"))?;
    let msg_cell = msg
        .serialize()
        .map_err(|e| anyhow::anyhow!("failed to serialize Message cell: {e}"))?;

    let cells = serialize_cells_tree_root_first(&msg_cell)
        .map_err(|e| anyhow::anyhow!("failed to flatten cell tree: {e}"))?;
    let entries = validate_layout(cells).context("BOC layout validation failed")?;

    let event = decode_event(&entries).context("event field decoding failed")?;

    let event_message_hash_hex = hex::encode(entries[0].repr_hash);
    let entries_ser: [CellRecord; 4] = [
        flat_to_record(&entries[0]),
        flat_to_record(&entries[1]),
        flat_to_record(&entries[2]),
        flat_to_record(&entries[3]),
    ];

    Ok(PrivateWitness {
        schema_version: SCHEMA_VERSION,
        event_message_hash_hex,
        block_id_hex: hex::encode(ctx.block_id),
        block_seq_no: ctx.block_seq_no,
        event,
        entries: entries_ser,
        block_context: BlockContext {
            account_dapp_id_hex: hex::encode(ctx.account_dapp_id),
            account_id_hex: hex::encode(ctx.account_id),
            envelope_hash_hex: hex::encode(ctx.envelope_hash),
        },
        events_tree_proof: None,
        block_tree_proof: None,
        anchor: None,
    })
}

fn flat_to_record(c: &FlatCell) -> CellRecord {
    CellRecord {
        repr_hash_hex: hex::encode(c.repr_hash),
        refs_count: c.refs_count,
        childs_repr_hashes_offset: c.childs_repr_hashes_offset.clone(),
        cell_repr_data_hex: hex::encode(&c.cell_repr_data),
    }
}
