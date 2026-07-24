//! Real Poseidon Merkle chain proof construction from actual block data.
//!
//! Builds genuine chain proofs by reconstructing layer-1 Poseidon trees from
//! intermediate key blocks fetched via GraphQL.
//!
//! # Two chain topologies, dispatched by layer growth
//!
//! The daemon proves one key block every `W · P` blocks (`W = 128`,
//! `P = 4` → 512-block cadence). Circuit 2 requires the chain to end at the
//! target block's top-layer root: `chain_result == layer_hash_frs[num_layers-1]`.
//!
//! Two cases occur under this cadence:
//!
//! * **Same-layer bundle** (`num_layers == prev_num_layers`) — the common case
//!   (~31 out of every 32 bundles). Chain **exactly `P = 4` layer-1 rungs** via
//!   [`build_chain_same_layer`], connecting the prev proved block's L1 root to
//!   the target block's `layer_hashes_preimage[0]` (also an L1 root). Each
//!   rung proves the L1 evolution over W blocks explicitly.
//!
//! * **New-layer bundle** (`num_layers == prev_num_layers + 1`) — fires at
//!   every `W^L` boundary (with L = 2 that is every W² = 16384 blocks, i.e.
//!   every 32nd bundle). Chain **one L(N+1) rung** via
//!   [`build_chain_for_new_layer`]: the L(N+1) tree at target holds the
//!   prev's L(N) root as one of its `W` data leaves (positions 2..W+2), and a
//!   single opening from that data-leaf position to the L(N+1) root satisfies
//!   the circuit's top-layer constraint. Under the W·P cadence contract the
//!   inclusion slot is deterministic: `prev_seqno = target - P·W`, so the
//!   prev-L(N) root always sits at leaf index `2 + (W - 1 - (P-1))` in the
//!   L(N+1) tree.
//!
//! Endpoints (both cases):
//!   * **start** = prev's max-level layer hash (`BridgeState::prev_max_level_layer_hash_for`)
//!   * **end**   = target's top-layer root (`layer_hash_frs[num_layers-1]`)
//!
//! # L1 tree layout per rung (W = 128)
//!   Leaf [0]:        higher_layer_root (layer 2 root at prior L2 boundary, or zero)
//!   Leaf [1]:        prev_same_layer_root (chain position; L1 root of the previous rung)
//!   Leaf [2..130]:   block leaves — Poseidon(block_id ‖ envelope_hash ‖ ext_msg_root) × W
//!   Leaf [130..256]: zero padding (to next power of 2)
//! Total: 256 leaves, depth 8.
//!
//! # L(N≥2) tree layout (W = 128)
//!   Leaf [0]:        higher_layer_root (layer N+1 root at prior boundary, or zero)
//!   Leaf [1]:        prev_same_layer_root (zero at first appearance of layer N)
//!   Leaf [2..130]:   layer-(N-1) roots from W past L(N-1)-keyblocks
//!   Leaf [130..256]: zero padding
//! Same shape and depth as L1 → `DenseChainLink` padding stays consistent.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context};
use gosh_dense_balanced_tree::DenseChainLink;
use tracing::info;

use crate::bridge_state::BridgeState;
use crate::chain_proof_builder::{
    self, build_chain_proofs, pad_leaves_to_power_of_2, LayerTreeData,
};
use bridge_gql_fetcher::gql_client::GqlClient;

/// Result of building chain proofs from real block data.
pub struct RealChainResult {
    /// Padded chain links (MAX_CHAIN_LEN entries, inactive ones at the end).
    pub chain_links: Vec<DenseChainLink>,
    /// Number of active chain steps (1..=MAX_CHAIN_LEN).
    pub num_steps: u8,
    /// The starting hash for the chain (prev_max_level_layer_hash bytes).
    pub prev_hash: [u8; 32],
}

/// Build real Poseidon chain proofs from prev's top-layer root to target's
/// top-layer root (`layer_hash_frs[num_layers - 1]`).
///
/// Dispatches on layer growth between prev and target:
///
/// * `num_layers == prev_num_layers` — same-layer path, `P` L1 rungs via
///   [`build_chain_same_layer`].
/// * `num_layers == prev_num_layers + 1` — new-layer path, one L(N+1) rung
///   via [`build_chain_for_new_layer`] (the prev L(N) root is included as a
///   data leaf in the target's L(N+1) tree).
///
/// A gap of ≥ 2 between prev and target layer counts means the state silently
/// skipped an intervening bundle — under W·P cadence at most one W^L boundary
/// can be crossed per bundle, so this is a hard consistency error and bails.
pub async fn build_real_chain(
    gql: &GqlClient,
    state: &BridgeState,
    target_history_proofs: &BTreeMap<u8, [u8; 32]>,
    target_seqno: u64,
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    let num_layers = target_history_proofs.len();
    ensure!(num_layers >= 1, "target block has no history_proofs");
    let prev_num_layers = state.num_active_layers();

    // start = prev's max-level layer hash (same value the verifier will
    // reconstruct from its BridgeState via `prev_max_level_layer_hash_for`).
    let prev_hash = state.prev_max_level_layer_hash_for(num_layers);

    // end = target's top-layer root (= layer_hash_frs[num_layers-1]).
    let target_hash = *target_history_proofs
        .get(&(num_layers as u8))
        .ok_or_else(|| anyhow::format_err!(
            "target block missing top layer {} in history_proofs",
            num_layers
        ))?;

    info!(
        "building real chain: num_layers={}, prev_num_layers={}, prev_top={}, target_top={}",
        num_layers,
        prev_num_layers,
        hex::encode(prev_hash),
        hex::encode(target_hash),
    );

    if num_layers > prev_num_layers {
        // Case B: a new layer just appeared at target (e.g. FIRST L2 boundary
        // with prev_num_layers=1, num_layers=2). Prev's L(N) root sits at a
        // data-leaf slot of target's L(N+1) tree — single opening.
        // Under W·P cadence only one boundary can be crossed per bundle; a
        // strict-+1 gap is enforced inside `build_chain_for_new_layer`.
        build_chain_for_new_layer(
            gql,
            target_seqno,
            prev_num_layers,
            num_layers,
            prev_hash,
            window_size,
        )
        .await
    } else if num_layers >= 2 && target_seqno % window_size.pow(num_layers as u32) == 0 {
        // Case D: subsequent L(N) boundary (target_seqno aligned to W^N) with
        // prev already at num_layers=N. Prev's L(N) root sits at slot 1 of
        // target's L(N) tree (`prev_same_layer_root`) — single opening.
        build_chain_same_layer_n(gql, target_seqno, num_layers as u8, prev_hash, window_size).await
    } else {
        // Cases A and C: same-layer L1 path — chain P L1 rungs from prev_L1 to
        // target_L1. Note `prev_hash` here equals prev's L1 latest because
        // under W·P cadence non-boundary bundles always emit num_layers=1, and
        // `prev_max_level_layer_hash_for(1)` returns L1 latest regardless of
        // how many higher layers prev has accumulated.
        build_chain_same_layer(gql, state, target_seqno, prev_hash, window_size).await
    }
}

/// Same-layer chain at layer N (N >= 2): one rung from prev_L(N) to target_L(N)
/// via slot-1 opening in target's L(N) tree.
async fn build_chain_same_layer_n(
    gql: &GqlClient,
    target_seqno: u64,
    layer: u8,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    info!(
        "building L{} same-layer chain (subsequent boundary): target_seq={}, prev_L{}={}",
        layer,
        target_seqno,
        layer,
        hex::encode(prev_hash),
    );

    let tree = build_layer_n_tree(gql, target_seqno, layer, prev_hash, window_size)
        .await
        .with_context(|| format!("building layer {} tree at seq={}", layer, target_seqno))?;

    let (chain_links, num_steps) = build_chain_proofs(&[tree]);

    Ok(RealChainResult {
        chain_links,
        num_steps,
        prev_hash,
    })
}

/// Build the L1 chain of P rungs connecting prev's L1 root to target's L1 root.
///
/// `step_size` is fixed to `window_size` (= W) because chain_layer = 1 is
/// baked into the cadence contract. `prev_seqno` and `target_seqno` must both
/// be W-aligned; under the daemon's W·P cadence they are always W·P-aligned,
/// which is strictly stronger.
async fn build_chain_same_layer(
    gql: &GqlClient,
    state: &BridgeState,
    target_seqno: u64,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    let step_size = window_size; // L1 step
    let prev_seqno = state.stored_last_seen_block_seq_no;

    // Enforce the invariants the caller is expected to uphold. Baking them
    // into runtime `ensure!` calls turns silent chain-corruption modes (mis-
    // aligned prev_seqno was previously masked by a dead `seq % step_size == 0`
    // filter that quietly produced a single-hop chain skipping every
    // intermediate step) into loud bails.
    ensure!(
        prev_seqno < target_seqno,
        "prev_seqno {} must be strictly less than target_seqno {}",
        prev_seqno,
        target_seqno,
    );
    ensure!(
        prev_seqno % step_size == 0,
        "prev_seqno {} not aligned to L1 step {} (bridge state stored a \
         non-key-block seqno)",
        prev_seqno,
        step_size,
    );
    ensure!(
        target_seqno % step_size == 0,
        "target_seqno {} not aligned to L1 step {}",
        target_seqno,
        step_size,
    );

    // With prev_seqno and target_seqno both known to be multiples of step_size
    // and prev < target, the walk from prev+step to target is a straight
    // arithmetic sequence — no modulo filter, no tail rescue, no empty case.
    // Under W·P cadence this yields exactly P rungs.
    let mut key_seqnos = Vec::new();
    let mut seq = prev_seqno + step_size;
    while seq <= target_seqno {
        key_seqnos.push(seq);
        seq += step_size;
    }
    debug_assert!(
        !key_seqnos.is_empty() && *key_seqnos.last().unwrap() == target_seqno,
        "L1 walk should end at target_seqno by construction",
    );

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
        "L1 chain: {} rungs, seqnos={:?}",
        key_seqnos.len(),
        key_seqnos
    );

    // Build a LayerTreeData for each L1 rung.
    let mut trees = Vec::with_capacity(key_seqnos.len());
    let mut chain_leaf_value = prev_hash;

    for &key_seq in &key_seqnos {
        let tree = build_layer1_tree(gql, key_seq, chain_leaf_value, window_size)
            .await
            .with_context(|| format!("building layer 1 tree at seq={}", key_seq))?;

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

/// Build chain when a new layer appeared at target.
///
/// Layer N first materialises at block height `W^N` (e.g. with W = 128, layer 2
/// first appears at block 16384, layer 3 at 2^21 = 2_097_152, etc.).
///
/// **Single-step by construction.** Under W·P thinning cadence the prover
/// advances the bridge state fast enough that we can only ever cross one layer
/// boundary between updates. A gap ≥ 2 would mean the state silently missed an
/// intervening update — a hard consistency error we refuse to paper over with a
/// longer chain. The `ensure!` below turns that invariant into a runtime bail
/// so a misconfigured operator sees it immediately instead of shipping a proof
/// against a stale state.
async fn build_chain_for_new_layer(
    gql: &GqlClient,
    target_seqno: u64,
    prev_num_layers: usize,
    num_layers: usize,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    ensure!(
        num_layers == prev_num_layers + 1,
        "gap {} between prev_num_layers={} and num_layers={} — thinning cadence \
         only permits gap=1; a larger jump means the bridge state skipped an update",
        num_layers - prev_num_layers,
        prev_num_layers,
        num_layers,
    );
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
    let position = leaves
        .iter()
        .enumerate()
        .find_map(|(i, l)| (i >= 2 && *l == prev_hash).then_some(i))
        .ok_or_else(|| {
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
    // Higher layer root (layer 2): the layer-2 root from the PREVIOUS L2 key
    // block (strictly < key_block_seqno). Strict `<` avoids self-reference at
    // an L2-boundary block, where the L2 root is derived from this very layer-1
    // tree and cannot appear as leaves[0].
    let higher_root = {
        let l2_step = window_size * window_size;
        let prev_l2_block = key_block_seqno
            .checked_sub(1)
            .map(|s| (s / l2_step) * l2_step)
            .unwrap_or(0);
        if prev_l2_block > 0 {
            fetch_layer_root(gql, prev_l2_block, 2).await.unwrap_or([0u8; 32])
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

/// Build a layer N (N>=2) Poseidon tree for a key block, with the chain leaf
/// positioned at slot 1 (`prev_same_layer_root`).
///
/// Used by [`build_chain_same_layer_n`] for subsequent L(N) boundaries where
/// prev already has an L(N) root — the target's L(N) tree carries prev's L(N)
/// root at slot 1, so a single opening from slot 1 to the L(N) root chains
/// prev_L(N) → target_L(N) in one rung.
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
///
/// Used by [`build_chain_for_new_layer`] (chain leaf at a data-leaf position)
/// and [`build_layer_n_tree`] (chain leaf at slot 1).
async fn build_layer_n_leaves(
    gql: &GqlClient,
    key_block_seqno: u64,
    layer: u8,
    prev_same_root: [u8; 32],
    window_size: u64,
) -> anyhow::Result<Vec<[u8; 32]>> {
    // Higher layer root: layer-(N+1) root from the PREVIOUS higher-layer key
    // block (strictly < key_block_seqno). At a layer-(N+1) boundary the layer-
    // (N+1) root is derived from this very layer-N tree, so leaves[0] must
    // reference the PRIOR boundary, not this one.
    let higher_layer = layer + 1;
    let higher_step = window_size.pow(higher_layer as u32);
    let prev_higher_seqno = key_block_seqno
        .checked_sub(1)
        .map(|s| (s / higher_step) * higher_step)
        .unwrap_or(0);
    let higher_root = if prev_higher_seqno > 0 {
        fetch_layer_root(gql, prev_higher_seqno, higher_layer)
            .await
            .unwrap_or([0u8; 32])
    } else {
        [0u8; 32]
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
    Ok(crate::poseidon_dense::compute_block_leaf_hash(
        &block.block_id,
        &block.envelope_hash,
        &block.tracked_ext_out_messages_root,
    ))
}
