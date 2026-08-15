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
//! * **New-layer bundle** (`num_layers > prev_num_layers`) — fires at every
//!   `W^L` boundary (with L = 2 that is every W² = 16384 blocks, i.e. every
//!   32nd bundle; L = 3 every W³ = 2_097_152 blocks; etc.). Chain **G =
//!   num_layers − prev_num_layers rungs vertically** (one rung per new layer)
//!   via [`build_chain_for_new_layer`]:
//!     * Rung 1 — target's L(prev_num_layers+1) tree holds prev's
//!       L(prev_num_layers) root as one of its `W` data leaves (positions
//!       2..W+2); opening at that data-leaf position produces target's
//!       L(prev_num_layers+1) root.
//!     * Rungs 2..G — each intermediate L(L) tree at target carries target's
//!       L(L−1) root at its LAST data-leaf position (index `2 + W − 1`),
//!       because canonical construction places `latest_layer_root(L−1)` at
//!       `data_leaves[W−1]` and target itself is the "latest" L(L−1)
//!       boundary. Each opening walks up exactly one layer.
//!   Under steady-state W·P cadence G is always 1 (single-layer jump per
//!   bundle). G ≥ 2 only occurs on fresh mid-chain bootstrap that lands at a
//!   compound boundary (e.g. seeding `layers=1` right before an L3 boundary,
//!   which is also an L2 and L1 boundary — 1→3 jump). It is not a
//!   production-cadence scenario but is required for historical-replay
//!   testing.
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
//!   Leaf [0]:        higher_layer_root (layer N+1 root at prior L(N+1) boundary, or zero if none yet on chain)
//!   Leaf [1]:        prev_same_layer_root (layer N root at prior L(N) boundary = `target - W^N`;
//!                    zero ONLY when `target == W^N`, i.e., very first L(N) boundary ever on chain).
//!                    NOTE: this is a property of the chain's tree construction; it is unrelated
//!                    to whether the bridge state has observed any prior L(N) boundary.
//!   Leaf [2..130]:   layer-(N-1) roots from W past L(N-1)-keyblocks
//!   Leaf [130..256]: zero padding
//! Same shape and depth as L1 → `DenseChainLink` padding stays consistent.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context};
use gosh_dense_balanced_tree::DenseChainLink;
use tracing::{info, warn};

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
/// * `num_layers > prev_num_layers` — new-layer path, G = gap rungs walked
///   vertically via [`build_chain_for_new_layer`]. G = 1 is the steady-state
///   case (prev L(N) root sits at a data leaf of target's L(N+1) tree). G ≥ 2
///   only occurs on fresh mid-chain bootstrap landing at a compound boundary
///   (e.g. seeding `layers=1` right before an L3 boundary — which is also an
///   L2 and L1 boundary, giving a 1→3 jump). Under W·P steady-state cadence
///   at most one W^L boundary can be crossed per bundle, so G ≥ 2 is logged
///   loudly as it usually means fresh-bootstrap; a mid-run occurrence would
///   indicate the bridge state skipped an update.
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

/// Build chain when one or more new layers appeared at target.
///
/// Layer N first materialises at block height `W^N` (with W = 128 → L2 at
/// 16384, L3 at 2^21 = 2_097_152, etc.). At a compound boundary (e.g. an L3
/// boundary block, which is simultaneously an L2 and L1 boundary) more than
/// one new layer can appear between prev and target — this happens when the
/// bridge is bootstrapped fresh mid-chain and its first bundle lands at such
/// a compound boundary. Under steady-state W·P cadence bridge state advances
/// fast enough to only ever cross one new layer per bundle (G = 1).
///
/// Walks the chain **vertically** with `G = num_layers − prev_num_layers`
/// rungs, one per new layer:
///
///   * Rung 1 (`layer = prev_num_layers + 1`): chain leaf = `prev_hash`, at
///     the data-leaf position where prev's L(prev_num_layers) root appears in
///     target's L(prev_num_layers+1) tree. Under W·P cadence
///     `prev_seqno = target − P·W`, so the inclusion slot is deterministic
///     (leaf index `2 + (W − 1 − (P − 1))`).
///   * Rungs 2..G (`layer = prev_num_layers + 1 + k`): chain leaf = target's
///     L(layer−1) root, at the LAST data-leaf position (index `2 + W − 1`).
///     By canonical construction of `data_leaves` in
///     [`build_layer_n_leaves`], `data_leaves[i]` is `fetch_layer_root(target
///     − (W − 1 − i)·W^(layer−1), layer−1)`, so `data_leaves[W−1]` is
///     `fetch_layer_root(target, layer−1)` — i.e., target's L(layer−1) root
///     itself.
///
/// `build_chain_proofs` then produces one `DenseChainLink` per tree; Circuit
/// 2 enforces `link[k+1].chain_leaf_value == link[k].computed_root`,
/// stitching the vertical walk together automatically.
async fn build_chain_for_new_layer(
    gql: &GqlClient,
    target_seqno: u64,
    prev_num_layers: usize,
    num_layers: usize,
    prev_hash: [u8; 32],
    window_size: u64,
) -> anyhow::Result<RealChainResult> {
    ensure!(
        num_layers > prev_num_layers,
        "build_chain_for_new_layer called without new layer: prev_num_layers={}, num_layers={}",
        prev_num_layers,
        num_layers,
    );
    let gap = num_layers - prev_num_layers;
    if gap >= 2 {
        warn!(
            "multi-layer jump G={} (prev_num_layers={} → num_layers={}) at seq={} — \
             not a steady-state W·P cadence scenario; expected only on fresh mid-chain \
             bootstrap landing at a compound boundary. In production this would indicate \
             the bridge state skipped an intervening update.",
            gap, prev_num_layers, num_layers, target_seqno,
        );
    }

    info!(
        "building vertical chain: {} rung(s), prev_num_layers={} → num_layers={} at seq={}",
        gap, prev_num_layers, num_layers, target_seqno,
    );

    let mut trees: Vec<LayerTreeData> = Vec::with_capacity(gap);
    let mut chain_leaf_value = prev_hash;

    for step in 0..gap {
        let layer_num = (prev_num_layers + 1 + step) as u8;

        // Slot 1 (prev_same_layer_root) for this layer. See `build_layer_n_leaves`
        // and the acki-nacki `HistoryBlockData::calculate_root_hash` reference:
        // slot 1 = latest_layer_root(layer_num) = L(layer_num) root at the
        // previous L(layer_num) boundary on chain (= target − W^layer_num).
        // Non-zero whenever the chain has produced any prior L(layer_num)
        // boundary; zero only when target ≤ W^layer_num.
        let same_layer_step = window_size.pow(layer_num as u32);
        let prev_same_root = if target_seqno > same_layer_step {
            let prev_same_seqno = target_seqno - same_layer_step;
            fetch_layer_root(gql, prev_same_seqno, layer_num)
                .await
                .with_context(|| {
                    format!(
                        "fetching prev L{} root from block {} for L{} tree at seq={} \
                         (vertical-chain rung {}/{})",
                        layer_num, prev_same_seqno, layer_num, target_seqno,
                        step + 1, gap,
                    )
                })?
        } else {
            [0u8; 32]
        };

        info!(
            "  rung {}/{}: L{} tree at seq={}, prev_same_root={} (prev L{} boundary={})",
            step + 1, gap,
            layer_num,
            target_seqno,
            hex::encode(prev_same_root),
            layer_num,
            target_seqno.saturating_sub(same_layer_step),
        );

        let leaves = build_layer_n_leaves(
            gql,
            target_seqno,
            layer_num,
            prev_same_root,
            window_size,
        )
        .await
        .with_context(|| format!(
            "building L{} leaves at seq={} (vertical-chain rung {}/{})",
            layer_num, target_seqno, step + 1, gap,
        ))?;

        let position = if step == 0 {
            // First rung: chain leaf = prev_hash sits at whichever data-leaf
            // slot matches. Under W·P cadence this is deterministic; we search
            // rather than compute to catch cadence misalignments loudly.
            leaves
                .iter()
                .enumerate()
                .find_map(|(i, l)| (i >= 2 && *l == chain_leaf_value).then_some(i))
                .ok_or_else(|| {
                    anyhow::format_err!(
                        "prev_hash {} not found among L{} tree data leaves at seq={} \
                         (rung 1/{})",
                        hex::encode(chain_leaf_value),
                        layer_num,
                        target_seqno,
                        gap,
                    )
                })?
        } else {
            // Subsequent rungs: chain leaf = target's L(layer_num−1) root,
            // which by construction sits at data_leaves[W−1] = leaves[2+W−1].
            let last_data_position = 2 + window_size as usize - 1;
            ensure!(
                leaves[last_data_position] == chain_leaf_value,
                "L{} tree last data leaf {} != expected chain leaf {} at seq={} \
                 (rung {}/{}); GQL data anomaly or canonical construction mismatch",
                layer_num,
                hex::encode(leaves[last_data_position]),
                hex::encode(chain_leaf_value),
                target_seqno,
                step + 1,
                gap,
            );
            last_data_position
        };

        info!(
            "  rung {}/{}: chain_leaf at position {} of {} leaves",
            step + 1, gap, position, leaves.len(),
        );

        trees.push(LayerTreeData {
            leaves,
            chain_leaf_position: position,
            chain_leaf_value,
        });

        // Seed next rung's chain leaf = target's L(layer_num) root
        // (= this tree's computed root by construction). Fetching rather than
        // computing avoids re-doing Poseidon Merkle work and gives an
        // independent check against the tree we just built (Circuit 2 will
        // still enforce `link[k+1].chain_leaf == link[k].root`).
        if step + 1 < gap {
            chain_leaf_value = fetch_layer_root(gql, target_seqno, layer_num)
                .await
                .with_context(|| format!(
                    "fetching target's L{} root at seq={} for vertical-chain rung {}/{} seeding",
                    layer_num, target_seqno, step + 2, gap,
                ))?;
        }
    }

    let (chain_links, num_steps) = build_chain_proofs(&trees);

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
pub async fn build_layer1_tree(
    gql: &GqlClient,
    key_block_seqno: u64,
    chain_leaf_value: [u8; 32],
    window_size: u64,
) -> anyhow::Result<LayerTreeData> {
    // Higher layer root (layer 2): the layer-2 root from the PREVIOUS L2 key
    // block (strictly < key_block_seqno). Strict `<` avoids self-reference at
    // an L2-boundary block, where the L2 root is derived from this very layer-1
    // tree and cannot appear as leaves[0].
    //
    // When `prev_l2_block > 0` the chain HAS a non-zero L2 root there and the
    // fetch MUST succeed — bail loudly on any GQL/data anomaly rather than
    // silently substituting zero (which would build a tree that fails Circuit 2
    // verification with an opaque error, mirroring the Case B `prev_same_root`
    // bug that surfaced at shellnet 2,523,136).
    let higher_root = {
        let l2_step = window_size * window_size;
        let prev_l2_block = key_block_seqno
            .checked_sub(1)
            .map(|s| (s / l2_step) * l2_step)
            .unwrap_or(0);
        if prev_l2_block > 0 {
            fetch_layer_root(gql, prev_l2_block, 2)
                .await
                .with_context(|| {
                    format!(
                        "fetching prev L2 root from block {} for L1 tree at seq={}",
                        prev_l2_block, key_block_seqno
                    )
                })?
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
    //
    // When `prev_higher_seqno > 0` the chain HAS a non-zero L(N+1) root there
    // and the fetch MUST succeed — bail loudly on any GQL/data anomaly rather
    // than silently substituting zero (same class of silent-zero bug as the
    // Case B `prev_same_root` fix).
    let higher_layer = layer + 1;
    let higher_step = window_size.pow(higher_layer as u32);
    let prev_higher_seqno = key_block_seqno
        .checked_sub(1)
        .map(|s| (s / higher_step) * higher_step)
        .unwrap_or(0);
    let higher_root = if prev_higher_seqno > 0 {
        fetch_layer_root(gql, prev_higher_seqno, higher_layer)
            .await
            .with_context(|| {
                format!(
                    "fetching prev L{} root from block {} for L{} tree at seq={}",
                    higher_layer, prev_higher_seqno, layer, key_block_seqno
                )
            })?
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

// ============================================================================
// Event-anchor chain building
// ============================================================================
//
// The functions below build a chain that anchors a *single event* (not a
// bundle transition) into one of the verifier's `layer_windows[]` slots. The
// event's own L1 root is the chain's start, and the chain terminates at some
// L(target_layer) root the verifier already mirrors.
//
// Two topologies are supported here:
//
// * `target_layer = 1` — horizontal L1 walk from `H_e` (event's W-aligned
//   L1 KB) to `K` (the next W·P-aligned KB, which the verifier stores). The
//   chain has ≤ P−1 active rungs. Wait time budget: up to W·P−1 blocks after
//   the event before its K is proven.
//
// * `target_layer = 2` — one vertical L1→L2 rung. The event's L1 root is
//   opened at the appropriate data-leaf slot of the L2 tree at `T_2`
//   (⌈event_seq/W²⌉·W²), and the tree root equals the verifier's mirrored
//   L2 root at T_2. Wait time budget: up to W²−1 blocks after the event.
//
// Higher target_layer (3+) is future work: extend by chaining additional
// vertical rungs L2→L3, L3→L4, etc. Not implemented in this cut.

/// Return value of [`build_event_anchor_chain`].
pub struct EventAnchorChainResult {
    /// Active links only (unpadded). The caller pads to `MAX_CHAIN_LEN` with
    /// `DenseChainLink::inactive(final_chain_root, link_depth)`.
    pub active_links: Vec<DenseChainLink>,
    /// Root at the end of the chain. Should equal the verifier's mirrored
    /// L(target_layer) root at `anchor_kb_seqno` when all inputs are consistent.
    pub final_chain_root: [u8; 32],
    /// Depth of each link's Merkle proof (padded L1/L(N) tree depth).
    pub link_depth: usize,
    /// Verifier-side anchor block seqno: `K` for target_layer=1,
    /// `T_2` for target_layer=2.
    pub anchor_kb_seqno: u64,
}

/// Build the event-anchoring chain from an event's L1 root up to the chosen
/// verifier-stored layer root. See the module-level docs for the topology.
///
/// This is the single composed entry point used by
/// `bridge-event-witness-builder`; it deliberately keeps
/// [`build_layer_n_leaves`] / [`build_layer_n_tree`] private inside this
/// module so downstream code has a narrow, stable surface.
pub async fn build_event_anchor_chain(
    gql: &GqlClient,
    event_seq: u64,
    event_l1_root: [u8; 32],
    target_layer: u8,
    window_size: u64,
    thinning_factor_p: u64,
) -> anyhow::Result<EventAnchorChainResult> {
    let w = window_size;
    match target_layer {
        1 => build_event_anchor_chain_l1(gql, event_seq, event_l1_root, w, thinning_factor_p).await,
        2 => build_event_anchor_chain_l2(gql, event_seq, event_l1_root, w).await,
        n => bail!(
            "target_layer={} not yet supported (only 1 and 2 in this cut). \
             L(N≥3) anchoring is future work — see build_event_anchor_chain docs.",
            n
        ),
    }
}

/// Pure boundary math for L1 event anchoring.
///
/// Given an event's block seq_no, returns the triple `(H_e, K, hops)`:
/// * `H_e = ⌈event_seq/W⌉·W` — the W-aligned L1 tree containing the event.
/// * `K = ⌈event_seq/(W·P)⌉·(W·P)` — the thinned L1 anchor the verifier stores.
/// * `hops = (K − H_e) / W` — forward-hop rung count between them.
///
/// Invariants: `H_e ≤ K`, `(K − H_e) % W == 0`, `hops < P`.
///
/// Extracted so unit tests can pin the arithmetic without a live GQL
/// client. The live builder ([`build_event_anchor_chain_l1`]) consumes
/// exactly this triple.
fn l1_anchor_boundaries(event_seq: u64, w: u64, p: u64) -> (u64, u64, u64) {
    let h_e = (event_seq / w) * w + w;
    let k = (event_seq / (w * p)) * (w * p) + (w * p);
    let hops = (k - h_e) / w;
    (h_e, k, hops)
}

/// Pure boundary math for L2 event anchoring.
///
/// Returns `(H_e, T_2, position)`:
/// * `H_e = ⌈event_seq/W⌉·W` — the event's W-aligned L1 tree.
/// * `T_2 = ⌈event_seq/W²⌉·W²` — the L2 tree the event lives in.
/// * `position` — data-leaf index of `H_e` inside the L2 tree:
///   `position = 2 + (W − 1 − k)` where `k = (T_2 − H_e)/W`. The `+2`
///   offset accounts for the first two L2 leaves being
///   `[higher_layer_root, prev_same_layer_root]` (see the module-level
///   layout doc). `k < W` always holds because `H_e ∈ (T_2 − W², T_2]`.
///
/// Extracted for the same reason as [`l1_anchor_boundaries`].
fn l2_anchor_boundaries(event_seq: u64, w: u64) -> (u64, u64, usize) {
    let h_e = (event_seq / w) * w + w;
    let w2 = w * w;
    let t2 = (event_seq / w2) * w2 + w2;
    let k = (t2 - h_e) / w;
    let position = 2 + (w - 1 - k) as usize;
    (h_e, t2, position)
}

/// Horizontal L1 walk from `H_e` to `K` (both W-aligned; K is W·P-aligned).
/// Emits `hops = (K − H_e)/W` active rungs.
async fn build_event_anchor_chain_l1(
    gql: &GqlClient,
    event_seq: u64,
    event_l1_root: [u8; 32],
    w: u64,
    p: u64,
) -> anyhow::Result<EventAnchorChainResult> {
    let (h_e, k, hops) = l1_anchor_boundaries(event_seq, w, p);

    let mut chain_leaf_value = event_l1_root;
    let mut active_links: Vec<DenseChainLink> = Vec::with_capacity(hops as usize);
    let mut link_depth: usize = 0;

    for i in 1..=hops {
        let seq_i = h_e + i * w;
        let tree = build_layer1_tree(gql, seq_i, chain_leaf_value, w)
            .await
            .with_context(|| {
                format!("building L1 tree at seq={seq_i} for forward-hop {i}/{hops}")
            })?;
        let (root_i, siblings) =
            chain_proof_builder::build_tree_and_proof(&tree.leaves, tree.chain_leaf_position);
        link_depth = siblings.len();
        info!(
            "  L1 hop {}/{}: tree at seq={}, chain_pos={}, root={}",
            i, hops, seq_i, tree.chain_leaf_position, hex::encode(root_i),
        );
        active_links.push(DenseChainLink {
            active: true,
            siblings,
            position: tree.chain_leaf_position,
            leaf_native: chain_leaf_value,
        });
        chain_leaf_value = root_i;
    }

    // When hops == 0 we produced no rungs and no tree; the caller is
    // expected to pass in the block_tree_proof depth for inactive padding.
    // We report `0` here to signal "not observed"; the caller must fall
    // back to its own known link depth in that case.
    Ok(EventAnchorChainResult {
        active_links,
        final_chain_root: chain_leaf_value,
        link_depth,
        anchor_kb_seqno: k,
    })
}

/// Vertical L1→L2 rung: open the event's L1 root at its data-leaf position
/// inside the L2 tree at `T_2 = ⌈event_seq/W²⌉·W²`.
async fn build_event_anchor_chain_l2(
    gql: &GqlClient,
    event_seq: u64,
    event_l1_root: [u8; 32],
    w: u64,
) -> anyhow::Result<EventAnchorChainResult> {
    let (h_e, t2, position) = l2_anchor_boundaries(event_seq, w);
    let w2 = w * w;
    // Runtime sanity — `l2_anchor_boundaries` is pure and unit-tested, so
    // these hold by construction. Kept as defence-in-depth in case
    // upstream ever passes a degenerate `w` (e.g. 0 or non-power-of-two).
    ensure!(
        h_e <= t2 && (t2 - h_e) % w == 0,
        "internal: H_e={} not W-aligned inside L2 tree at T_2={} (W={})",
        h_e, t2, w,
    );
    let k = (t2 - h_e) / w; // hop count from H_e forward to T_2 at L1 stride
    ensure!(
        k < w,
        "internal: L2 window offset k={} exceeds W-1 (W={}) — event outside T_2's L2 window",
        k, w,
    );

    // Fetch the L2 tree's prev_same_layer_root (L2 root at T_2 - W², or zero
    // when T_2 == W²).
    let prev_same_root = if t2 > w2 {
        let prev_l2_seq = t2 - w2;
        fetch_layer_root(gql, prev_l2_seq, 2)
            .await
            .with_context(|| {
                format!(
                    "fetching prev L2 root from block {} for L2 tree at T_2={} \
                     (vertical L1→L2 rung)",
                    prev_l2_seq, t2,
                )
            })?
    } else {
        [0u8; 32]
    };

    let leaves = build_layer_n_leaves(gql, t2, 2, prev_same_root, w)
        .await
        .with_context(|| format!("building L2 leaves at T_2={} for vertical L1→L2 rung", t2))?;

    ensure!(
        position < leaves.len(),
        "internal: L2 leaf position {} out of bounds (len={})",
        position, leaves.len(),
    );
    if leaves[position] != event_l1_root {
        bail!(
            "L2 tree data leaf at position {} = {} does not match self-computed \
             R_1@H_e = {}. Either GQL data drifted since block_tree_proof was \
             built, or H_e = {} does not correspond to this L2 window (T_2 = {}).",
            position,
            hex::encode(leaves[position]),
            hex::encode(event_l1_root),
            h_e, t2,
        );
    }

    let (root, siblings) = chain_proof_builder::build_tree_and_proof(&leaves, position);
    let link_depth = siblings.len();
    info!(
        "L1→L2 rung: L2 tree at T_2={}, H_e={} at data-leaf position {} (k={}), \
         computed L2 root = {}",
        t2, h_e, position, k, hex::encode(root),
    );

    let active_links = vec![DenseChainLink {
        active: true,
        siblings,
        position,
        leaf_native: event_l1_root,
    }];

    Ok(EventAnchorChainResult {
        active_links,
        final_chain_root: root,
        link_depth,
        anchor_kb_seqno: t2,
    })
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

#[cfg(test)]
mod tests {
    //! Pure-arithmetic tests for the event-anchor boundary helpers. The
    //! live GQL-consuming paths (`build_event_anchor_chain_l1`/`_l2`) are
    //! covered by the manual E2E in `E2E_RUN_STATE_2026_08_15.md`; adding
    //! a mock GqlClient is deferred (would need a trait refactor).
    use super::{l1_anchor_boundaries, l2_anchor_boundaries};

    // Production values on shellnet: W = 128, P = 4.
    const W: u64 = 128;
    const P: u64 = 4;

    // ---- L1 -----------------------------------------------------------

    #[test]
    fn l1_event_at_zero_snaps_to_first_tree() {
        // event_seq = 0 → H_e = W, K = W·P (both smallest positive boundaries).
        let (h_e, k, hops) = l1_anchor_boundaries(0, W, P);
        assert_eq!(h_e, W);
        assert_eq!(k, W * P);
        assert_eq!(hops, P - 1); // (W·P − W) / W = P − 1
    }

    #[test]
    fn l1_event_exactly_on_h_e_still_climbs_next_tree() {
        // event_seq == W is *inside* the L1 tree at 2W (⌈W/W⌉·W = W … wait,
        // actually H_e = (W/W)*W + W = 2W). The event sits at the tail of
        // tree 2W. Bumped-to-next-W behaviour is intentional — matches the
        // circuit's convention that H_e is *strictly greater than* the
        // event block's seq.
        let (h_e, k, hops) = l1_anchor_boundaries(W, W, P);
        assert_eq!(h_e, 2 * W);
        assert_eq!(k, W * P);
        assert_eq!(hops, P - 2);
    }

    #[test]
    fn l1_event_exactly_on_k_lands_next_thinned_anchor() {
        // event_seq == W·P → H_e = W·P + W, K = 2·W·P.
        let (h_e, k, hops) = l1_anchor_boundaries(W * P, W, P);
        assert_eq!(h_e, W * P + W);
        assert_eq!(k, 2 * W * P);
        assert_eq!(hops, P - 1);
    }

    #[test]
    fn l1_shellnet_run_2026_08_15_boundaries() {
        // Regression pin for the 2026-08-15 shellnet E2E: event at
        // seq_no=8251308 → H_e = K = 8251392, hops = 0 (event happened
        // to land on a W·P-aligned tree).
        let (h_e, k, hops) = l1_anchor_boundaries(8_251_308, W, P);
        assert_eq!(h_e, 8_251_392);
        assert_eq!(k, 8_251_392);
        assert_eq!(hops, 0);
    }

    #[test]
    fn l1_hops_bounded_by_p_minus_one() {
        // For any event_seq, hops = (K − H_e)/W ∈ [0, P − 1]. Sweep an
        // arbitrary window to double-check the invariant holds.
        for offset in 0..(W * P * 3) {
            let (h_e, k, hops) = l1_anchor_boundaries(offset, W, P);
            assert_eq!(k % (W * P), 0, "K must be W·P-aligned (event_seq={offset})");
            assert_eq!(h_e % W, 0, "H_e must be W-aligned (event_seq={offset})");
            assert!(h_e <= k, "H_e must not exceed K (event_seq={offset})");
            assert!(hops < P, "hops < P must hold (event_seq={offset}, hops={hops})");
            assert_eq!((k - h_e) / W, hops);
        }
    }

    // ---- L2 -----------------------------------------------------------

    #[test]
    fn l2_event_at_zero_snaps_to_first_l2_tree() {
        // event_seq = 0 → H_e = W, T_2 = W², k = (W² − W)/W = W − 1.
        // position = 2 + (W − 1 − (W − 1)) = 2 (the first data-leaf slot,
        // corresponding to the earliest L1 tree in this L2 window).
        let (h_e, t2, position) = l2_anchor_boundaries(0, W);
        assert_eq!(h_e, W);
        assert_eq!(t2, W * W);
        assert_eq!(position, 2);
    }

    #[test]
    fn l2_event_at_last_l1_tree_in_window_lands_on_final_slot() {
        // event_seq = W² − W − 1 → H_e = W² − W → T_2 = W², k = 1,
        // position = 2 + (W − 1 − 1) = W. This is the *last* data-leaf
        // slot before the L2 tree pads to next-pow2.
        let event_seq = W * W - W - 1;
        let (h_e, t2, position) = l2_anchor_boundaries(event_seq, W);
        assert_eq!(h_e, W * W - W);
        assert_eq!(t2, W * W);
        assert_eq!(position, W as usize);
    }

    #[test]
    fn l2_event_at_w_squared_climbs_to_next_l2_tree() {
        // event_seq = W² → H_e = W² + W, T_2 = 2·W², k = W − 1,
        // position = 2 + 0 = 2. Event is the "just-past-boundary" case:
        // it's the first block in the new L2 window, and its L1 tree sits
        // at the earliest data-leaf slot (position 2) of the L2 tree at 2·W².
        let (h_e, t2, position) = l2_anchor_boundaries(W * W, W);
        assert_eq!(h_e, W * W + W);
        assert_eq!(t2, 2 * W * W);
        assert_eq!(position, 2);
    }

    #[test]
    fn l2_position_invariants_across_full_window() {
        // Sweep across an entire L2 window and verify position ∈ [2, W+1].
        let base = 5 * W * W; // arbitrary later window
        for offset in 0..(W * W) {
            let (h_e, t2, position) = l2_anchor_boundaries(base + offset, W);
            assert_eq!(t2 % (W * W), 0, "T_2 must be W²-aligned");
            assert_eq!(h_e % W, 0, "H_e must be W-aligned");
            assert!(h_e <= t2, "H_e must not exceed T_2");
            assert!(
                position >= 2 && position <= (W + 1) as usize,
                "position {} out of [2, W+1] (event_seq={})",
                position, base + offset,
            );
        }
    }
}
