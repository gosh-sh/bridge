//! Real Poseidon Merkle chain proof construction from actual block data.
//!
//! Builds genuine chain proofs by reconstructing layer Poseidon trees from
//! intermediate key blocks fetched via GraphQL.
//!
//! Tree structure per layer per window (HISTORY_PROOF_WINDOW_SIZE=128, the prod value):
//!   Leaf [0]:        higher_layer_root (layer N+1 root, or zero)
//!   Leaf [1]:        prev_same_layer_root (chain position for same-layer linking)
//!   Leaf [2..130]:   data leaves (layer 1: block leaves; layer 2+: lower-layer roots) — 128 slots
//!   Leaf [130..256]: zero padding (to next power of 2)
//! Total: 256 leaves, depth 8.
//!
//! Generalised: for any HISTORY_PROOF_WINDOW_SIZE = W the layout is
//!   [higher_layer_root, prev_same_layer_root, data×W, zero-pad to next pow2(W+2)].

use std::collections::BTreeMap;

use anyhow::{bail, Context};
use gosh_dense_balanced_tree::DenseChainLink;
use tracing::info;

use crate::bridge_state::BridgeState;
use crate::chain_proof_builder::{
    self, build_chain_proofs, pad_leaves_to_power_of_2, LayerTreeData,
};
use crate::gql_client::GqlClient;

/// Result of building chain proofs from real block data.
pub struct RealChainResult {
    /// Padded chain links (MAX_CHAIN_LEN entries, inactive ones at the end).
    pub chain_links: Vec<DenseChainLink>,
    /// Number of active chain steps (1..=MAX_CHAIN_LEN).
    pub num_steps: u8,
    /// The starting hash for the chain (prev_max_level_layer_hash bytes).
    pub prev_hash: [u8; 32],
}

/// Build real Poseidon chain proofs connecting prev_max_level_layer_hash
/// to the highest active layer hash in the target key block.
///
/// Fetches intermediate key blocks from the node to reconstruct Poseidon
/// Merkle trees and extract chain proof siblings.
pub async fn build_real_chain(
    gql: &GqlClient,
    state: &BridgeState,
    target_history_proofs: &BTreeMap<u8, [u8; 32]>,
    target_seqno: u64,
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    let num_layers = target_history_proofs.len();
    if num_layers == 0 {
        bail!("target block has no history_proofs");
    }

    let prev_num_layers = state.num_active_layers();
    let prev_hash = state.prev_max_level_layer_hash_for(num_layers);
    let target_hash = *target_history_proofs
        .get(&(num_layers as u8))
        .ok_or_else(|| anyhow::format_err!("missing layer {} in history_proofs", num_layers))?;

    info!(
        "building real chain: prev_layers={}, new_layers={}, prev_hash={}, target_hash={}",
        prev_num_layers,
        num_layers,
        hex::encode(prev_hash),
        hex::encode(target_hash),
    );

    // Determine if a TRULY new layer appeared (never seen before).
    // A layer re-appearing after being absent is NOT a new layer — it uses
    // the same-layer chain.
    //
    // Example with W = 128: L2 first appears at block W^2 = 16384, then again
    // at 2·W^2 = 32768. Between those L2 boundaries the key blocks at
    // 16512, 16640, …, 32640 are L1-only. The L2 boundary at 32768 is a
    // re-appearance, not a brand-new layer, so it takes the same-layer path.
    //
    // `prev_num_layers` is the highest populated 1-based layer index — see
    // `BridgeState::num_active_layers` for why count == max-index here.
    let new_layer_appeared = num_layers > prev_num_layers && prev_num_layers > 0;

    if new_layer_appeared {
        // New layer appeared: single step where prev_hash is a DATA leaf
        // in the new layer's tree (not at position 1).
        build_chain_for_new_layer(
            gql,
            target_seqno,
            num_layers,
            prev_hash,
            window_size,
        )
        .await
    } else {
        // Same layer count (or first proof): chain at the highest layer level.
        build_chain_same_layer(
            gql,
            state,
            target_seqno,
            num_layers,
            prev_hash,
            window_size,
        )
        .await
    }
}

/// Build chain when operating at the same layer level (most common case).
///
/// Lists intermediate key blocks between prev and target at the chain layer's
/// granularity, reconstructs each tree, and builds the chain.
async fn build_chain_same_layer(
    gql: &GqlClient,
    state: &BridgeState,
    target_seqno: u64,
    num_layers: usize,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    let chain_layer = num_layers as u8;

    let step_size = window_size.pow(chain_layer as u32);
    let prev_seqno = state.stored_last_seen_block_seq_no;

    // List key block seqnos at this layer from prev+step to target (inclusive).
    let mut key_seqnos = Vec::new();
    let mut seq = prev_seqno + step_size;
    while seq <= target_seqno {
        if seq % step_size == 0 {
            key_seqnos.push(seq);
        }
        seq += step_size;
    }
    // Ensure target is included (it should be, since we only process at key block boundaries).
    if key_seqnos.last() != Some(&target_seqno) && target_seqno % step_size == 0 {
        key_seqnos.push(target_seqno);
    }

    if key_seqnos.is_empty() {
        bail!(
            "no intermediate key blocks found between {} and {} at step_size={}",
            prev_seqno,
            target_seqno,
            step_size
        );
    }

    if key_seqnos.len() > gosh_dense_balanced_tree::MAX_CHAIN_LEN {
        bail!(
            "chain too long: {} steps exceeds MAX_CHAIN_LEN={}. Gap: {}-{}",
            key_seqnos.len(),
            gosh_dense_balanced_tree::MAX_CHAIN_LEN,
            prev_seqno,
            target_seqno,
        );
    }

    info!(
        "chain at layer {}: {} steps, seqnos={:?}",
        chain_layer,
        key_seqnos.len(),
        key_seqnos
    );

    // Build a LayerTreeData for each step.
    let mut trees = Vec::with_capacity(key_seqnos.len());
    let mut chain_leaf_value = prev_hash;

    for &key_seq in &key_seqnos {
        let tree = if chain_layer == 1 {
            build_layer1_tree(gql, key_seq, chain_leaf_value, window_size)
                .await
                .with_context(|| format!("building layer 1 tree at seq={}", key_seq))?
        } else {
            build_layer_n_tree(
                gql,
                key_seq,
                chain_layer,
                chain_leaf_value,
                window_size,
            )
            .await
            .with_context(|| format!("building layer {} tree at seq={}", chain_layer, key_seq))?
        };

        // The root of this tree becomes the chain_leaf_value for the next step.
        let (root, _) = chain_proof_builder::build_tree_and_proof(&tree.leaves, tree.chain_leaf_position);

        // Log leaves and computed root for debugging tree reconstruction.
        info!("  tree at seq={}: {} leaves, chain_pos={}", key_seq, tree.leaves.len(), tree.chain_leaf_position);
        for (i, l) in tree.leaves.iter().enumerate() {
            info!("    leaf[{}]: {}", i, hex::encode(l));
        }
        info!("    computed_root: {}", hex::encode(root));
        chain_leaf_value = root;

        trees.push(tree);
    }

    let (chain_links, num_steps) = build_chain_proofs(&trees);

    Ok(RealChainResult {
        chain_links,
        num_steps,
        prev_hash,
    })
}

/// Build chain when a new layer appeared.
///
/// Layer N first materialises at block height `W^N` (e.g. with W = 128, layer 2
/// first appears at block 16384, layer 3 at 2^21 = 2_097_152, etc.).
///
/// Single step: build the new layer's tree and find prev_hash among its data leaves.
async fn build_chain_for_new_layer(
    gql: &GqlClient,
    target_seqno: u64,
    num_layers: usize,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    let new_layer = num_layers as u8;

    info!(
        "new layer {} appeared at seq={}, finding prev_hash in data leaves",
        new_layer, target_seqno
    );

    // Build the new layer's tree. The prev_same_layer_root (position 1) is zero
    // (first occurrence of this layer). prev_hash should be among the data leaves.
    let leaves = build_layer_n_leaves(
        gql,
        target_seqno,
        new_layer,
        [0u8; 32], // prev_same_layer_root = zero (first occurrence)
        window_size,
    )
    .await?;

    // Find which data leaf matches prev_hash.
    let mut chain_pos = None;
    for (i, leaf) in leaves.iter().enumerate() {
        if *leaf == prev_hash && i >= 2 {
            chain_pos = Some(i);
            break;
        }
    }

    let position = chain_pos.ok_or_else(|| {
        anyhow::format_err!(
            "prev_hash {} not found among layer {} tree data leaves at seq={}",
            hex::encode(prev_hash),
            new_layer,
            target_seqno,
        )
    })?;

    info!(
        "found prev_hash at position {} in layer {} tree",
        position, new_layer
    );

    let tree = LayerTreeData {
        leaves,
        chain_leaf_position: position,
        chain_leaf_value: prev_hash,
    };

    let (chain_links, num_steps) = build_chain_proofs(&[tree]);

    Ok(RealChainResult {
        chain_links,
        num_steps,
        prev_hash,
    })
}

/// Build a layer 1 Poseidon tree for a key block.
///
/// Leaf layout (see crate-level docs for the concrete W=128 example):
///   [0]:         higher_layer_root (layer 2 root from this block, or zero)
///   [1]:         prev_same_layer_root = chain_leaf_value (from previous key block)
///   [2..W+2]:    Poseidon(block_id || envelope_hash || ext_msg_root) for the
///                HISTORY_PROOF_WINDOW_SIZE (W) blocks in the window
///   [W+2..pow2]: zero padding (to next power of 2)
async fn build_layer1_tree(
    gql: &GqlClient,
    key_block_seqno: u64,
    chain_leaf_value: [u8; 32],
    window_size: u64,
) -> anyhow::Result<LayerTreeData> {
    // Higher layer root (layer 2): the MOST RECENT L2 root, not necessarily from
    // this block. The node stores the latest higher-layer root in HistoryBlockData.
    // We find it by checking the current block first, then scanning back to the
    // most recent L2 key block (multiples of window_size^2).
    let higher_root = {
        let l2_step = window_size * window_size;
        let most_recent_l2_block = (key_block_seqno / l2_step) * l2_step;
        if most_recent_l2_block > 0 {
            fetch_layer_root(gql, most_recent_l2_block, 2).await.unwrap_or([0u8; 32])
        } else {
            [0u8; 32]
        }
    };

    // Fetch block metadata for the HISTORY_PROOF_WINDOW_SIZE (W) blocks in this window.
    // Window for layer 1 at height H: blocks [H - W, ..., H - 1].
    // The key block itself is NOT in the window — the window contains
    // the W blocks BEFORE the key block.
    let window_start = key_block_seqno - window_size;

    let mut data_leaves = Vec::with_capacity(window_size as usize);
    for seq in window_start..window_start + window_size {
        // Decode the full Envelope<AckiNackiBlock> from the boc field to get
        // the exact block_id, envelope_hash, and ext_messages_root the node uses.
        let leaf = fetch_block_leaf_hash_from_boc(gql, seq)
            .await
            .with_context(|| format!("fetching block leaf hash for block {}", seq))?;
        info!("  block {} leaf: {}", seq, hex::encode(leaf));
        data_leaves.push(leaf);
    }

    // Assemble leaves.
    let mut leaves = Vec::with_capacity(2 + window_size as usize + 2);
    leaves.push(higher_root);
    leaves.push(chain_leaf_value);
    leaves.extend_from_slice(&data_leaves);
    pad_leaves_to_power_of_2(&mut leaves);

    Ok(LayerTreeData {
        leaves,
        chain_leaf_position: 1,
        chain_leaf_value,
    })
}

/// Build a layer N (N>=2) Poseidon tree for a key block.
///
/// Leaf layout (see crate-level docs for the concrete W=128 example):
///   [0]:        higher_layer_root (layer N+1 root, or zero)
///   [1]:        prev_same_layer_root = chain_leaf_value
///   [2..W+2]:   layer N-1 root hashes from HISTORY_PROOF_WINDOW_SIZE (W) intermediate key blocks
///   [W+2..pow2]: zero padding (to next power of 2)
async fn build_layer_n_tree(
    gql: &GqlClient,
    key_block_seqno: u64,
    layer: u8,
    chain_leaf_value: [u8; 32],
    window_size: u64,
) -> anyhow::Result<LayerTreeData> {
    let leaves = build_layer_n_leaves(
        gql,
        key_block_seqno,
        layer,
        chain_leaf_value,
        window_size,
    )
    .await?;

    Ok(LayerTreeData {
        leaves,
        chain_leaf_position: 1,
        chain_leaf_value,
    })
}

/// Build leaf array for a layer N (N>=2) tree.
async fn build_layer_n_leaves(
    gql: &GqlClient,
    key_block_seqno: u64,
    layer: u8,
    prev_same_root: [u8; 32],
    window_size: u64,
) -> anyhow::Result<Vec<[u8; 32]>> {
    // Higher layer root.
    let higher_layer = layer + 1;
    let higher_root = if key_block_seqno == 0 {
        [0u8; 32]
    } else {
        fetch_layer_root(gql, key_block_seqno, higher_layer)
            .await
            .unwrap_or([0u8; 32])
    };

    // Data leaves: layer (N-1) roots from HISTORY_PROOF_WINDOW_SIZE (W) key blocks.
    // The step size for layer N-1 is window_size^(N-1).
    let lower_layer = layer - 1;
    let lower_step = window_size.pow(lower_layer as u32);

    // The HISTORY_PROOF_WINDOW_SIZE (W) key blocks contributing to this layer N tree:
    // [key_block_seqno - (W-1)*lower_step, ..., key_block_seqno - lower_step, key_block_seqno]
    let mut data_leaves = Vec::with_capacity(window_size as usize);
    for i in 0..window_size {
        let offset = (window_size - 1 - i) * lower_step;
        let intermediate_seq = key_block_seqno - offset;

        let lower_root = fetch_layer_root(gql, intermediate_seq, lower_layer)
            .await
            .with_context(|| {
                format!(
                    "fetching layer {} root from block {} for layer {} tree",
                    lower_layer, intermediate_seq, layer
                )
            })?;
        data_leaves.push(lower_root);
    }

    // Assemble leaves.
    let mut leaves = Vec::with_capacity(2 + window_size as usize + 2);
    leaves.push(higher_root);
    leaves.push(prev_same_root);
    leaves.extend_from_slice(&data_leaves);
    pad_leaves_to_power_of_2(&mut leaves);

    Ok(leaves)
}

/// Fetch a specific layer's root hash from a block's `history_proofs` via GQL.
///
/// `pub` so integration tests and downstream binaries can probe individual
/// layer roots without going through the full chain builder.
pub async fn fetch_layer_root(gql: &GqlClient, seqno: u64, layer: u8) -> anyhow::Result<[u8; 32]> {
    let block = gql.query_proof_block_by_seqno(seqno).await?;
    block
        .history_proofs
        .get(&layer)
        .copied()
        .ok_or_else(|| anyhow::format_err!("block {} has no layer {} in history_proofs", seqno, layer))
}

/// Fetch a block's leaf hash directly from GQL (block_id, envelope_hash, ext_out_root).
async fn fetch_block_leaf_hash_from_boc(gql: &GqlClient, seqno: u64) -> anyhow::Result<[u8; 32]> {
    let block = gql.query_proof_block_by_seqno(seqno).await?;
    Ok(block.block_leaf_hash())
}
