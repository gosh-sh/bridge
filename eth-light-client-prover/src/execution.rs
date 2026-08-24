//! M3-exec — `hash_tree_root(ExecutionPayloadHeader)` + execution-branch, binding
//! the finalized **execution `block_hash`** to the finalized beacon block.
//!
//! This is the output `finalizeDeposit` consumes: the deposit references an L1
//! execution block hash, and the light client must prove that hash is the one
//! inside the finalized beacon block (which the step circuit proves is finalized).
//!
//! `ExecutionPayloadHeader` (Deneb..Fulu, 17 fields) merkleization:
//!
//! | # | field | SSZ type | root |
//! |---|-------|----------|------|
//! | 0 | parent_hash | Bytes32 | node |
//! | 1 | fee_recipient | Bytes20 | `bytes_root` (1 chunk) |
//! | 2 | state_root | Bytes32 | node |
//! | 3 | receipts_root | Bytes32 | node |
//! | 4 | logs_bloom | Bytes256 | `bytes_root` (8 chunks) |
//! | 5 | prev_randao | Bytes32 | node |
//! | 6 | block_number | uint64 | `uint64_root` |
//! | 7 | gas_limit | uint64 | `uint64_root` |
//! | 8 | gas_used | uint64 | `uint64_root` |
//! | 9 | timestamp | uint64 | `uint64_root` |
//! | 10 | extra_data | List[byte,32] | `mix_in_length(chunk, len)` |
//! | 11 | base_fee_per_gas | uint256 | 32 LE bytes |
//! | 12 | **block_hash** | Bytes32 | node (exposed) |
//! | 13 | transactions_root | Bytes32 | node |
//! | 14 | withdrawals_root | Bytes32 | node |
//! | 15 | blob_gas_used | uint64 | `uint64_root` |
//! | 16 | excess_blob_gas | uint64 | `uint64_root` |
//!
//! 17 fields → padded to 32. `execution_branch` proves this root against
//! `beacon.body_root` at `EXECUTION_PAYLOAD_GINDEX = 25` (depth 4).

use crate::ssz::{
    container_root, load_bytes, load_node, native_bytes_root, native_container_root,
    native_sha256_pair, native_uint64_root, sha256_pair, uint64_root, Node,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::utils::BigPrimeField;
use halo2_base::Context;

/// Generalized index of `execution_payload` in `BeaconBlockBody` for the
/// LightClientHeader (Capella..Fulu). `floor(log2(25)) = 4` → 4-element branch.
pub const EXECUTION_PAYLOAD_GINDEX: u64 = 25;

/// Native `ExecutionPayloadHeader` fields (Deneb..Fulu).
#[derive(Clone)]
pub struct ExecutionPayloadVals {
    pub parent_hash: [u8; 32],
    pub fee_recipient: [u8; 20],
    pub state_root: [u8; 32],
    pub receipts_root: [u8; 32],
    pub logs_bloom: Vec<u8>, // 256
    pub prev_randao: [u8; 32],
    pub block_number: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub extra_data: Vec<u8>, // <= 32
    pub base_fee_per_gas: [u8; 32], // uint256 little-endian
    pub block_hash: [u8; 32],
    pub transactions_root: [u8; 32],
    pub withdrawals_root: [u8; 32],
    pub blob_gas_used: u64,
    pub excess_blob_gas: u64,
}

// ---------------------------------------------------------------------------
// In-circuit
// ---------------------------------------------------------------------------

/// `uint64_root` of a native `u64` (loads the 8 LE bytes, then pads to 32).
fn u64_root<F: BigPrimeField>(ctx: &mut Context<F>, v: u64) -> Node<F> {
    let bytes = load_bytes(ctx, &v.to_le_bytes());
    uint64_root(ctx, &bytes)
}

/// `hash_tree_root(ExecutionPayloadHeader)`. Returns `(root, block_hash_node)` so
/// the caller can bind/expose the execution block hash.
pub fn execution_payload_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    p: &ExecutionPayloadVals,
) -> (Node<F>, Node<F>) {
    assert_eq!(p.logs_bloom.len(), 256, "logs_bloom must be 256 bytes");
    assert!(p.extra_data.len() <= 32, "extra_data exceeds 32 bytes");

    let f0 = load_node(ctx, &p.parent_hash);
    let f1 = {
        let b = load_bytes(ctx, &p.fee_recipient);
        crate::ssz::bytes_root(chip, ctx, &b)
    };
    let f2 = load_node(ctx, &p.state_root);
    let f3 = load_node(ctx, &p.receipts_root);
    let f4 = {
        let b = load_bytes(ctx, &p.logs_bloom);
        crate::ssz::bytes_root(chip, ctx, &b)
    };
    let f5 = load_node(ctx, &p.prev_randao);
    let f6 = u64_root(ctx, p.block_number);
    let f7 = u64_root(ctx, p.gas_limit);
    let f8 = u64_root(ctx, p.gas_used);
    let f9 = u64_root(ctx, p.timestamp);
    let f10 = {
        // List[byte,32]: mix_in_length(single-chunk, len)
        let mut chunk = load_bytes(ctx, &p.extra_data);
        while chunk.len() < 32 {
            chunk.push(ctx.load_constant(F::ZERO));
        }
        let len_node = u64_root(ctx, p.extra_data.len() as u64);
        sha256_pair(chip, ctx, &chunk, &len_node)
    };
    let f11 = load_node(ctx, &p.base_fee_per_gas);
    let f12 = load_node(ctx, &p.block_hash);
    let f13 = load_node(ctx, &p.transactions_root);
    let f14 = load_node(ctx, &p.withdrawals_root);
    let f15 = u64_root(ctx, p.blob_gas_used);
    let f16 = u64_root(ctx, p.excess_blob_gas);

    let block_hash_node = f12.clone();
    let root = container_root(
        chip,
        ctx,
        vec![
            f0, f1, f2, f3, f4, f5, f6, f7, f8, f9, f10, f11, f12, f13, f14, f15, f16,
        ],
    );
    (root, block_hash_node)
}

// ---------------------------------------------------------------------------
// Native reference
// ---------------------------------------------------------------------------

pub fn native_execution_payload_root(p: &ExecutionPayloadVals) -> [u8; 32] {
    assert_eq!(p.logs_bloom.len(), 256);
    assert!(p.extra_data.len() <= 32);

    let extra_root = {
        let mut chunk = [0u8; 32];
        chunk[..p.extra_data.len()].copy_from_slice(&p.extra_data);
        let len_node = native_uint64_root(p.extra_data.len() as u64);
        native_sha256_pair(&chunk, &len_node)
    };

    native_container_root(vec![
        p.parent_hash,
        native_bytes_root(&p.fee_recipient),
        p.state_root,
        p.receipts_root,
        native_bytes_root(&p.logs_bloom),
        p.prev_randao,
        native_uint64_root(p.block_number),
        native_uint64_root(p.gas_limit),
        native_uint64_root(p.gas_used),
        native_uint64_root(p.timestamp),
        extra_root,
        p.base_fee_per_gas,
        p.block_hash,
        p.transactions_root,
        p.withdrawals_root,
        native_uint64_root(p.blob_gas_used),
        native_uint64_root(p.excess_blob_gas),
    ])
}
