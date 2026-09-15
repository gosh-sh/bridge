//! Real Poseidon Merkle chain proof construction from actual block data.
//!
//! Builds genuine chain proofs by reconstructing per-layer Poseidon trees
//! (L1 rungs for same-layer bundles, vertical L(N) rungs for new-layer
//! bundles) from intermediate key blocks fetched via GraphQL. See "Two
//! chain topologies, dispatched by layer growth" below for the split.
//!
//! # Two chain topologies, dispatched by layer growth
//!
//! The daemon proves one key block every `W · P` blocks (`W = 128`,
//! `P = 8` → 1024-block cadence). Circuit 2 requires the chain to end at the
//! target block's top-layer root: `chain_result == layer_hash_frs[num_layers-1]`.
//!
//! Two cases occur under this cadence:
//!
//! * **Same-layer bundle** (`num_layers == prev_num_layers`) — the common case
//!   (~15 out of every 16 bundles). Chain **exactly `P = 8` layer-1 rungs** via
//!   [`build_chain_same_layer`], connecting the prev proved block's L1 root to
//!   the target block's `layer_hashes_preimage[0]` (also an L1 root). Each
//!   rung proves the L1 evolution over W blocks explicitly.
//!
//! * **New-layer bundle** (`num_layers > prev_num_layers`) — fires at every
//!   `W^L` boundary (with L = 2 that is every W² = 16384 blocks, i.e. every
//!   16th bundle at P = 8; L = 3 every W³ = 2_097_152 blocks; etc.). Chain **G =
//!   num_layers − prev_num_layers rungs vertically** (one rung per new layer)
//!   via [`build_chain_for_new_layer`]:
//!     * Rung 1 — target's L(prev_num_layers+1) tree holds prev's
//!       L(prev_num_layers) root as one of its `W` data leaves (positions
//!       2..W+2); opening at that data-leaf position produces target's
//!       L(prev_num_layers+1) root.
//!     * Rungs 2..G — each intermediate L(L) tree at target carries target's
//!       L(L−1) root at its LAST data-leaf position (index `2 + W − 1`).
//!       The source of truth for this layout is the acki-nacki node's
//!       `HistoryBlockData::calculate_root_hash` (the reference cited at
//!       [`build_chain_for_new_layer`] line 368), which places the W L(L−1)
//!       contributions of a target-L(L) tree in ascending seq_no order —
//!       so target's own L(L−1) contribution lands in `data_leaves[W−1]` =
//!       `leaves[2 + W − 1]`. Our [`build_layer_n_leaves`] mirrors that
//!       order verbatim, and a runtime `ensure!` in
//!       [`build_chain_for_new_layer`] (around line 430) checks the
//!       correspondence for each rung — surfacing a loud parse error if
//!       the node ever changes the layout. Every rung `L−1 → L` opens at
//!       that fixed slot; each opening walks up exactly one layer.
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

use crate::bridge_state::{BridgeState, MAX_LAYERS};
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

    // Safety guard requested in `l2_anchoring_implementation_plan.md` §2.1
    // and hardened per PR #35 follow-up review: Case D (subsequent L(N)
    // boundary with N >= 2) is a single-rung horizontal hop by construction.
    // Anything else here means either the driver picked a mis-aligned target
    // (should have been caught by the % W^N check in the caller) or
    // `build_chain_proofs` disagreed with `build_layer_n_tree` on step count.
    // Was `debug_assert_eq!` — but release builds compiled it out, so a
    // mis-dispatch could silently feed a multi-step chain into Circuit 2
    // aggregator calldata. Promoted to `ensure!` so the invariant holds in
    // release too; caller (`chain_proof_builder`) propagates the error.
    ensure!(
        num_steps == 1,
        "Case D (L{} same-layer) must produce exactly 1 chain step, got {} \
         — driver dispatched Case D at a mis-aligned target or \
         build_chain_proofs disagreed with build_layer_n_tree on step count",
        layer, num_steps,
    );

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
    // Safety guard requested in `l2_anchoring_implementation_plan.md` §2.1:
    // this path is L1-only (`num_layers = 1`, `chain_steps = P`). If prev
    // state already has num_active_layers >= 2, the driver picked a target
    // that fell out of Case B (new-layer) and Case D (%W^N-aligned) yet the
    // state carries L2+ history — a classic "L2 daemon mis-picked target"
    // signature. Loud warn; not fatal because the walk itself is still
    // valid and downstream ensure!s enforce alignment. verifyBlock will
    // revert on-chain with PrevAnchorMismatch, which is recoverable, but
    // this log line lets an operator diagnose the mis-dispatch without
    // reading the Solidity revert data.
    let prev_num_layers = state.num_active_layers();
    if prev_num_layers >= 2 {
        warn!(
            "build_chain_same_layer (L1-only path) fired with num_active_layers={} — \
             expected 1. Driver likely mis-picked target under L2 anchoring; \
             expect on-chain PrevAnchorMismatch. target_seqno={}, prev_seqno={}",
            prev_num_layers,
            target_seqno,
            state.stored_last_seen_block_seq_no,
        );
    }

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
    // Same invariant class as the Case D guard above: if this fires in
    // release we have already committed to building an L1 chain whose last
    // rung does not land on the target block, and Circuit 2 will produce a
    // chain root that never matches the on-chain expected anchor. Was
    // `debug_assert!` — promoted to `ensure!` alongside the Case D guard so
    // both same-file invariants have the same strength in release builds.
    ensure!(
        !key_seqnos.is_empty() && key_seqnos.last().copied() == Some(target_seqno),
        "L1 walk did not end at target_seqno by construction: \
         prev_seqno={}, target_seqno={}, step_size={}, walk={:?}",
        prev_seqno, target_seqno, step_size, key_seqnos,
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
// * `target_layer = n` (2 ≤ n ≤ MAX_LAYERS) — a stack of `n − 1` vertical
//   rungs L1→L2, L2→L3, …, L(n−1)→L(n). Each rung `m → m+1` opens the
//   L(m) root at `T_m` inside the L(m+1) tree at `T_{m+1}`, where
//   `T_m = ⌈event_seq / W^m⌉·W^m` and `T_1 = H_e`. Wait time budget: up to
//   `W^n − 1` blocks after the event. Terminates at the L(n) root the
//   verifier mirrors at `T_n`.

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
    /// `T_n = ⌈event_seq / W^n⌉·W^n` for target_layer=n (n ≥ 2).
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
        n if (2..=MAX_LAYERS as u8).contains(&n) => {
            build_event_anchor_chain_ln(gql, event_seq, event_l1_root, n, w).await
        }
        n => bail!(
            "target_layer={} out of range: must be in 1..={} (MAX_LAYERS).",
            n, MAX_LAYERS,
        ),
    }
}

/// Pure boundary math for L1 event anchoring.
///
/// Given an event's block seq_no, returns the triple `(H_e, K, hops)`:
/// * `H_e = ⌊event_seq/W⌋·W + W` — seq_no of the KEY BLOCK where the L1
///   root of the batch containing `event_seq` is emitted. Per the History-
///   proofs proposal (§ "Construct the Layer 1 Batch Proof" and the
///   L1 example), batch `M` covers block heights `[M·W, (M+1)·W − 1]`
///   and its root `#L1(M)` is stored **in the common section of the
///   first block of batch M+1**, which sits at height `(M+1)·W`. With
///   `M = ⌊event_seq/W⌋`, that height is exactly what the code computes.
///   Note this is *not* `⌈event_seq/W⌉·W`: when `event_seq` is itself
///   the first block of a batch (a KB, `event_seq % W == 0`), the
///   ceiling formula returns `event_seq` itself — the tree whose root
///   sits *there* covers the previous batch `[event_seq − W, event_seq − 1]`
///   and does NOT include the event. We must advance to the next KB.
/// * `K = ⌊event_seq/(W·P)⌋·(W·P) + (W·P)` — seq_no of the next thinned
///   L1 anchor the verifier mirrors on-chain (the verifier stores L1
///   roots at every W·P-th KB, not every KB). Same "strictly next
///   multiple" pattern as `H_e`.
/// * `hops = (K − H_e) / W` — forward-hop rung count between them.
///
/// Invariants: `H_e ≤ K`, `(K − H_e) % W == 0`, `hops < P`.
///
/// Pub so callers (e.g. `bridge-event-witness-builder`'s auto-escalation
/// probe) can predict `K` without running the full chain builder. The
/// live builder ([`build_event_anchor_chain_l1`]) consumes exactly this
/// triple.
pub fn l1_anchor_boundaries(event_seq: u64, w: u64, p: u64) -> (u64, u64, u64) {
    let h_e = (event_seq / w) * w + w;
    let k = (event_seq / (w * p)) * (w * p) + (w * p);
    let hops = (k - h_e) / w;
    (h_e, k, hops)
}

/// Pure boundary math for L2 event anchoring.
///
/// Returns `(H_e, T_2, position)`:
/// * `H_e = ⌊event_seq/W⌋·W + W` — seq_no of the KEY BLOCK where the L1
///   root of the batch containing `event_seq` is emitted (see
///   [`l1_anchor_boundaries`] for the derivation).
/// * `T_2 = ⌊event_seq/W²⌋·W² + W²` — seq_no of the KEY BLOCK where the
///   L2 root of the L2-batch containing `event_seq` is emitted. Per the
///   proposal (§ "Layer 2" example), the L2 batch `M₂ = ⌊event_seq/W²⌋`
///   covers L1 batches `[M₂·W, (M₂+1)·W − 1]` (i.e. block heights
///   `[M₂·W², (M₂+1)·W² − 1]`), and its root `#L2(M₂)` lives in the
///   common section of the first block of L2-batch `M₂+1`, at seq_no
///   `(M₂+1)·W²`. Same "strictly next multiple" pattern as `H_e`: on an
///   exact `W²` boundary the code advances to the next KB, because the
///   root sitting at `event_seq` itself covers the *previous* L2 batch.
/// * `position` — data-leaf index of `H_e` inside the L2 tree at `T_2`:
///   `position = 2 + (W − 1 − k)` where `k = (T_2 − H_e)/W`. The `+2`
///   offset accounts for the first two L2 leaves being
///   `[higher_layer_root, prev_same_layer_root]`, matching the
///   chronological data-leaf layout of `HistoryBlockData::calculate_root_hash`
///   in the acki-nacki node. `k < W` always holds because
///   `H_e ∈ (T_2 − W², T_2]`.
///
/// Pub for the same reason as [`l1_anchor_boundaries`].
pub fn l2_anchor_boundaries(event_seq: u64, w: u64) -> (u64, u64, usize) {
    let h_e = (event_seq / w) * w + w;
    let w2 = w * w;
    let t2 = (event_seq / w2) * w2 + w2;
    let k = (t2 - h_e) / w;
    let position = 2 + (w - 1 - k) as usize;
    (h_e, t2, position)
}

/// Pure boundary math for L(n) event anchoring (n ≥ 2).
///
/// Returns the boundary stack `[T_1, T_2, …, T_n]`, each entry being the
/// seq_no of a KEY BLOCK (*not* a tree):
/// * `T_1 = H_e = ⌊event_seq/W⌋·W + W` — the KB emitting the L1 root of
///   the batch containing the event (see [`l1_anchor_boundaries`]).
/// * `T_m = ⌊event_seq/W^m⌋·W^m + W^m` for `m ≥ 2` — the KB emitting the
///   L(m) root of the L(m)-batch containing the event. This is the
///   recursive generalisation of the L1/L2 rule from the History-proofs
///   proposal: L(m)-batch `M_m = ⌊event_seq/W^m⌋` covers `W` consecutive
///   L(m−1) batches, and its root `#L(m)(M_m)` is emitted in the common
///   section of the first block of L(m)-batch `M_m + 1`, at seq_no
///   `(M_m + 1)·W^m`. Not `⌈event_seq/W^m⌉·W^m`: on an exact `W^m`
///   boundary the root at `event_seq` covers the *previous* L(m) batch,
///   so we advance to the next KB.
///
/// Each consecutive pair `(T_m, T_{m+1})` describes one vertical rung of
/// the L(n) chain: the L(m) root at `T_m` is opened at data-leaf position
/// `2 + (W − 1 − k_m)` of the L(m+1) tree at `T_{m+1}`, where
/// `k_m = (T_{m+1} − T_m) / W^m ∈ [0, W − 1]`. Chronological data-leaf
/// ordering follows `HistoryBlockData::calculate_root_hash` in the
/// acki-nacki node.
///
/// Panics on `n == 0`. The caller is expected to bound `n ≤ MAX_LAYERS`.
///
/// Pub for the same reason as [`l1_anchor_boundaries`]: auto-probe callers
/// need to predict `T_n` without running the full chain builder.
pub fn l_n_anchor_boundaries(event_seq: u64, w: u64, n: u8) -> Vec<u64> {
    assert!(n >= 1, "l_n_anchor_boundaries: n must be ≥ 1");
    let mut boundaries = Vec::with_capacity(n as usize);
    let mut step = w;
    for _ in 1..=n {
        let t = (event_seq / step) * step + step;
        boundaries.push(t);
        // `step *= w` may overflow past MAX_LAYERS but callers bound n.
        step = step.saturating_mul(w);
    }
    boundaries
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

/// Generalized vertical stack for L(n) event anchoring (n ≥ 2).
///
/// Walks `n − 1` rungs: L1@H_e → L2@T_2 → L3@T_3 → … → L(n)@T_n. Each rung
/// `m → m+1` builds the L(m+1) tree at `T_{m+1}`, verifies that its data
/// leaf at `position = 2 + (W − 1 − k_m)` matches the previously produced
/// root (defence-in-depth against GQL/data drift), then emits the Merkle
/// opening as a `DenseChainLink`.
///
/// Reduces to a single L1→L2 rung when `n == 2` (matches the prior
/// L2-only implementation byte-for-byte in the shape of `active_links`).
///
/// `n` is checked against `MAX_LAYERS` upstream (`build_event_anchor_chain`
/// dispatch); we assert it here as a defence-in-depth internal invariant.
async fn build_event_anchor_chain_ln(
    gql: &GqlClient,
    event_seq: u64,
    event_l1_root: [u8; 32],
    n: u8,
    w: u64,
) -> anyhow::Result<EventAnchorChainResult> {
    ensure!(
        (2..=MAX_LAYERS as u8).contains(&n),
        "internal: build_event_anchor_chain_ln called with n={} (must be 2..={})",
        n, MAX_LAYERS,
    );

    let boundaries = l_n_anchor_boundaries(event_seq, w, n);
    // boundaries[i-1] == T_i, so boundaries[0] = H_e (= T_1), boundaries[n-1] = T_n.
    let h_e = boundaries[0];
    let t_n = *boundaries.last().unwrap();

    let mut chain_leaf_value = event_l1_root;
    let mut active_links: Vec<DenseChainLink> = Vec::with_capacity((n - 1) as usize);
    let mut link_depth: usize = 0;
    let num_rungs = (n - 1) as usize;

    // Iterate rungs m = 1..n (target layer at each step is m+1).
    for m_idx in 0..num_rungs {
        let m = (m_idx + 1) as u8;
        let target_layer = m + 1;
        let t_m = boundaries[m_idx];
        let t_m1 = boundaries[m_idx + 1];
        // W^m (lower step) and W^(m+1) (this layer's step).
        let w_m: u64 = w.pow(m as u32);
        let w_m1: u64 = w.pow(target_layer as u32);

        ensure!(
            t_m <= t_m1 && (t_m1 - t_m) % w_m == 0,
            "internal: T_{}={} not W^{}-aligned inside L{} tree at T_{}={} (W={})",
            m, t_m, m, target_layer, target_layer, t_m1, w,
        );
        let k_m = (t_m1 - t_m) / w_m;
        ensure!(
            k_m < w,
            "internal: L{} rung offset k={} exceeds W-1 (W={}) — T_{}={} outside T_{}={}'s window",
            target_layer, k_m, w, m, t_m, target_layer, t_m1,
        );
        let position = 2 + (w - 1 - k_m) as usize;

        // Fetch this layer's prev_same_layer_root (root at T_{m+1} - W^{m+1},
        // or zero when T_{m+1} == W^{m+1} — the very first L(m+1) boundary).
        let prev_same_root = if t_m1 > w_m1 {
            let prev_seq = t_m1 - w_m1;
            fetch_layer_root(gql, prev_seq, target_layer)
                .await
                .with_context(|| {
                    format!(
                        "fetching prev L{} root from block {} for L{} tree at T_{}={} \
                         (vertical L{}→L{} rung, event_seq={})",
                        target_layer, prev_seq, target_layer, target_layer, t_m1,
                        m, target_layer, event_seq,
                    )
                })?
        } else {
            [0u8; 32]
        };

        let leaves = build_layer_n_leaves(gql, t_m1, target_layer, prev_same_root, w)
            .await
            .with_context(|| {
                format!(
                    "building L{} leaves at T_{}={} for vertical L{}→L{} rung",
                    target_layer, target_layer, t_m1, m, target_layer,
                )
            })?;

        ensure!(
            position < leaves.len(),
            "internal: L{} rung leaf position {} out of bounds (len={})",
            target_layer, position, leaves.len(),
        );
        if leaves[position] != chain_leaf_value {
            bail!(
                "L{} data leaf at position {} = {} does not match chain leaf {} \
                 (rung L{}→L{}, T_{}={}, T_{}={}, event_seq={}). Either GQL data \
                 drifted since the previous rung was built, or the boundary stack \
                 is inconsistent for this event.",
                target_layer, position,
                hex::encode(leaves[position]),
                hex::encode(chain_leaf_value),
                m, target_layer,
                m, t_m, target_layer, t_m1,
                event_seq,
            );
        }

        let (root, siblings) = chain_proof_builder::build_tree_and_proof(&leaves, position);
        link_depth = siblings.len();
        info!(
            "L{}→L{} rung: tree at T_{}={}, T_{}={} at data-leaf position {} (k={}), \
             computed L{} root = {}",
            m, target_layer, target_layer, t_m1, m, t_m, position, k_m,
            target_layer, hex::encode(root),
        );

        active_links.push(DenseChainLink {
            active: true,
            siblings,
            position,
            leaf_native: chain_leaf_value,
        });
        chain_leaf_value = root;
    }

    // Silence unused-var warning when n == 2 (h_e only appears in log context above).
    let _ = h_e;

    Ok(EventAnchorChainResult {
        active_links,
        final_chain_root: chain_leaf_value,
        link_depth,
        anchor_kb_seqno: t_n,
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
    use super::{l1_anchor_boundaries, l2_anchor_boundaries, l_n_anchor_boundaries};

    // W as in production; P = 4 is a smaller stride so the boundary
    // arithmetic is easier to read by hand. `THINNING_FACTOR_P` is 8.
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

    // ---- L(N) ---------------------------------------------------------

    #[test]
    fn ln_event_at_zero_boundaries_are_pure_powers_of_w() {
        // event_seq = 0 → T_i = W^i for i = 1..=5.
        let boundaries = l_n_anchor_boundaries(0, W, 5);
        assert_eq!(boundaries.len(), 5);
        assert_eq!(boundaries[0], W);
        assert_eq!(boundaries[1], W * W);
        assert_eq!(boundaries[2], W * W * W);
        assert_eq!(boundaries[3], W * W * W * W);
        // W^5 is well within u64 range for W = 128 (128^5 ≈ 3.4 × 10^10).
        assert_eq!(boundaries[4], W.pow(5));
    }

    #[test]
    fn ln_agrees_with_l1_and_l2_helpers() {
        // The dedicated L1 and L2 helpers are the authoritative reference
        // for the first two entries. Sweep a range of event seqs and check.
        for offset in [0u64, 1, W - 1, W, W + 1, W * W - 1, W * W, 5 * W * W + 42] {
            let boundaries = l_n_anchor_boundaries(offset, W, 3);
            let (h_e_l1, _, _) = l1_anchor_boundaries(offset, W, P);
            let (h_e_l2, t2, _) = l2_anchor_boundaries(offset, W);
            assert_eq!(boundaries[0], h_e_l1, "T_1 mismatch (event_seq={offset})");
            assert_eq!(boundaries[0], h_e_l2, "T_1 mismatch vs L2 (event_seq={offset})");
            assert_eq!(boundaries[1], t2, "T_2 mismatch (event_seq={offset})");
        }
    }

    #[test]
    fn ln_rung_invariants_across_full_l3_window() {
        // For n = 3 boundaries [T_1, T_2, T_3], each rung m→m+1 must satisfy
        // k_m = (T_{m+1} - T_m) / W^m ∈ [0, W-1], and position = 2 + (W-1-k_m)
        // must lie in [2, W+1]. Sweep an arbitrary L3 window.
        let base = 3 * W.pow(3); // arbitrary later L3 window
        // W^3 = 128^3 = 2_097_152 — too many to sweep exhaustively; take a
        // representative stride.
        let step = W * W / 8; // 2048 samples per L3 window
        let mut offset = 0u64;
        while offset < W.pow(3) {
            let boundaries = l_n_anchor_boundaries(base + offset, W, 3);
            for m_idx in 0..2 {
                let t_m = boundaries[m_idx];
                let t_m1 = boundaries[m_idx + 1];
                let w_m = W.pow((m_idx + 1) as u32);
                assert!(t_m <= t_m1, "T_{} > T_{} (event_seq={})", m_idx + 1, m_idx + 2, base + offset);
                assert_eq!(
                    (t_m1 - t_m) % w_m,
                    0,
                    "T_{} − T_{} not W^{}-aligned",
                    m_idx + 2, m_idx + 1, m_idx + 1,
                );
                let k_m = (t_m1 - t_m) / w_m;
                assert!(k_m < W, "k_{} = {} exceeds W-1 (event_seq={})", m_idx + 1, k_m, base + offset);
                let position = 2 + (W - 1 - k_m) as usize;
                assert!(
                    position >= 2 && position <= (W + 1) as usize,
                    "L{} rung position {} out of [2, W+1]",
                    m_idx + 2, position,
                );
            }
            offset += step;
        }
    }

    #[test]
    fn ln_event_on_w_cubed_boundary_climbs_next_l3_tree() {
        // event_seq = W³ exactly → T_1 = W³ + W, T_2 = W³ + W², T_3 = 2·W³.
        // Rung 1: k_1 = (T_2 - T_1)/W = (W² - W)/W = W - 1, position = 2.
        // Rung 2: k_2 = (T_3 - T_2)/W² = (W³ - W²)/W² = W - 1, position = 2.
        let w3 = W.pow(3);
        let boundaries = l_n_anchor_boundaries(w3, W, 3);
        assert_eq!(boundaries[0], w3 + W);
        assert_eq!(boundaries[1], w3 + W * W);
        assert_eq!(boundaries[2], 2 * w3);
    }
}
