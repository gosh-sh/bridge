//! Daemon-side enrichment of a partial [`PrivateWitness`] into a full one
//! that `bridge-event-halo2-prover --fixture` can consume.
//!
//! Formerly all of this logic lived inline in
//! `bin/build.rs`. It has been lifted into a library module so that
//! in-process orchestrators (the withdraw-E2E driver plumbed into
//! Sergey's relayer) can call it without shelling out.
//!
//! The `bridge-event-witness-builder` binary is now a thin CLI wrapper
//! over [`enrich_witness`] plus the ambient I/O (state file / partial
//! JSON / output path).
//!
//! ### Fields filled in
//!
//! * `events_tree_proof` — Poseidon Merkle proof from `ext_msg_leaf =
//!   Poseidon96(dapp || account || repr_hash)` up to the block's
//!   `ext_out_messages_root`.
//! * `block_tree_proof` — Poseidon Merkle proof from `block_leaf =
//!   Poseidon96(block_id || envelope_hash || ext_out_root)` up to `root_1` (the
//!   L1 window root the verifier mirrors).
//! * `anchor` — references the L(target_layer) hash the verifier mirrors, with
//!   a dense chain from H_e to the verifier-side key block.
//!
//! Behavior mirrors what the standalone binary used to do; the only
//! surface change is that `guard_wait_time` and `resolve_anchor_layer`
//! are now internal helpers with a stable public entrypoint.

use anyhow::{bail, Context, Result};
use bridge_event_prove_circuit::multi_hop_witness::{
    block_merkle_leaf_proof, proof_block_ref_inner_path_native, ref_leaf_hash_native,
    verify_block_merkle_leaf_proof, verify_proof_block_ref_inner_path, N_BUNDLE_MAX,
};
use bridge_gql_fetcher::{
    gql_client::{GqlClient, GqlProofBlock},
    types::ThreadIdentifier,
};
use bridge_prover_lib::{
    block_id_tree::BlockIdMerkleTree,
    bridge_state::{BridgeState, MAX_LAYERS},
    chain_proof_builder::{build_tree_and_proof, pad_leaves_to_power_of_2},
    real_chain_builder,
};
use gosh_dense_balanced_tree::{DenseChainLink, MAX_CHAIN_LEN};
use tracing::{info, warn};

use crate::schema::{
    AnchorRef, BlockWitnessJson, DenseChainLinkSer, HopWitnessJson, MerkleProofData,
    MultiHopBundleWitnessJson, MultiHopProofWitnessJson, PrivateWitness, SCHEMA_VERSION,
};

pub const HISTORY_WINDOW_SIZE: u64 =
    bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64;

pub const THINNING_FACTOR_P: u64 = bridge_prover_lib::THINNING_FACTOR_P;

/// Anchor-layer selection mode. See the `bin/build.rs` module docblock
/// for the full semantics of each variant.
///
/// * `Explicit(n)` — strict: use L(n), error out if the anchor's height has
///   rolled out of `layer_windows[n-1]`.
/// * `Auto` — try L1 first; if L1's K has rolled out of the L1 window, escalate
///   through L2, L3, …, L(num_active_layers) and take the first layer whose
///   window still covers the event. Implies opt-in to the wait budget of the
///   chosen layer.
#[derive(Debug, Clone, Copy)]
pub enum AnchorLayerMode {
    Explicit(u8),
    Auto,
}

/// Summary of one enrichment run — mirrors the JSON line the standalone
/// binary prints as its last stdout line. Owned strings so the caller
/// can serialize / stash it however it likes.
#[derive(Debug, Clone)]
pub struct EnrichSummary {
    pub schema_version: u32,
    pub event_message_hash_hex: String,
    pub block_seq_no: u64,
    /// Event's own W-aligned key block `H_e` (covers the block_tree_proof).
    pub key_block_seq_no: u64,
    /// Verifier-side anchor key block seqno. Named `thinned_*` for
    /// backwards compat with existing JSON consumers.
    pub thinned_key_block_seq_no: u64,
    /// 0-indexed layer (0 = L1, 1 = L2, …).
    pub layer_idx: u32,
    pub layer_hash_hex: String,
    pub events_tree_depth: usize,
    pub block_tree_depth: usize,
    pub num_active_chain_steps: u32,
    /// Whether `AnchorLayerMode::Auto` escalated past L1.
    pub auto_escalated: bool,
}

/// Enriched witness returned by [`enrich_witness`] — the mutated
/// `PrivateWitness` (with `events_tree_proof`, `block_tree_proof`,
/// `anchor` all populated) plus a summary the caller can log or persist.
#[derive(Debug, Clone)]
pub struct EnrichedWitness {
    pub witness: PrivateWitness,
    pub summary: EnrichSummary,
}

/// Enrich a partial `PrivateWitness` (from the hermetic exporter) with
/// daemon-side fields — the core function the standalone binary
/// `bridge-event-witness-builder` and any in-process orchestrator both
/// go through.
///
/// * `gql` — GraphQL client pointed at the AN node.
/// * `bridge_state` — already-loaded snapshot (usually
///   `state/prover_state.json`; the schema is shared with
///   `verifier_state.json`).
/// * `partial` — partial witness JSON from
///   [`crate::export_from_event_boc_base64`]. Consumed and returned in enriched
///   form.
/// * `anchor_mode` — see [`AnchorLayerMode`].
/// * `i_know_the_wait` — opt-in to expensive anchor layers when `anchor_mode =
///   Explicit(n)` and `n ≥ 2`. Ignored for L1. Auto mode counts as an implicit
///   opt-in.
pub async fn enrich_witness(
    gql: &GqlClient,
    bridge_state: &BridgeState,
    mut partial: PrivateWitness,
    anchor_mode: AnchorLayerMode,
    i_know_the_wait: bool,
) -> Result<EnrichedWitness> {
    if partial.schema_version != SCHEMA_VERSION {
        bail!(
            "partial witness schema_version={} but expected {SCHEMA_VERSION}",
            partial.schema_version,
        );
    }

    if !bridge_state.initialized {
        bail!("bridge state is uninitialized — no key blocks processed yet");
    }
    info!(
        "bridge state: window_size={}, active_layers={}, last_seen_seq_no={}, last_seen_height={}",
        bridge_state.window_size,
        bridge_state.num_active_layers(),
        bridge_state.stored_last_seen_block_seq_no,
        bridge_state.stored_last_seen_block_height,
    );

    let event_seq = partial.block_seq_no;
    let event_repr_hash = parse_hex32("event_message_hash_hex", &partial.event_message_hash_hex)?;
    let dapp = parse_hex32(
        "block_context.account_dapp_id_hex",
        &partial.block_context.account_dapp_id_hex,
    )?;
    let acc = parse_hex32(
        "block_context.account_id_hex",
        &partial.block_context.account_id_hex,
    )?;
    info!(
        "partial witness: event_repr_hash={}, block_seq_no={}",
        hex::encode(event_repr_hash),
        event_seq,
    );

    // Resolve anchor layer (auto probe or explicit passthrough).
    let (anchor_layer, escalated_from_auto) =
        resolve_anchor_layer(gql, bridge_state, event_seq, anchor_mode)
            .await
            .context("resolving anchor layer")?;
    info!(
        "resolved anchor: L{} (mode={:?}, auto_escalated={})",
        anchor_layer, anchor_mode, escalated_from_auto,
    );

    // Wait-time guard. Auto mode counts as implicit acknowledgement —
    // the caller opted in to escalation.
    let implicit_optin = matches!(anchor_mode, AnchorLayerMode::Auto);
    guard_wait_time(
        anchor_layer,
        i_know_the_wait || implicit_optin,
        HISTORY_WINDOW_SIZE,
    )?;

    // events_tree_proof.
    let events_tree_proof = build_events_tree_proof(gql, event_seq, &dapp, &acc, &event_repr_hash)
        .await
        .context("building events_tree_proof failed")?;
    info!(
        "events_tree_proof: position={}, depth={}",
        events_tree_proof.position,
        events_tree_proof.siblings_hex.len(),
    );

    // H_e (event's L1 key block).
    let w = HISTORY_WINDOW_SIZE;
    let p = THINNING_FACTOR_P;
    let key_block_seq = ((event_seq / w) * w) + w;
    let window_start = key_block_seq - w;
    let block_offset_in_window = event_seq - window_start;
    info!(
        "H_e={} (event's W-aligned KB, window=[{}..{}), offset={})",
        key_block_seq, window_start, key_block_seq, block_offset_in_window,
    );

    // block_tree_proof.
    let (block_tree_proof, l1_root_self_computed) =
        build_block_tree_proof(gql, key_block_seq, w, block_offset_in_window)
            .await
            .context("building block_tree_proof failed")?;
    info!(
        "block_tree_proof: position={}, depth={}, root_self_computed={}",
        block_tree_proof.position,
        block_tree_proof.siblings_hex.len(),
        hex::encode(l1_root_self_computed),
    );

    // Event-anchor chain (L1 horizontal or L(n≥2) vertical).
    let chain_result = real_chain_builder::build_event_anchor_chain(
        gql,
        event_seq,
        l1_root_self_computed,
        anchor_layer,
        w,
        p,
    )
    .await
    .with_context(|| {
        format!(
            "building event-anchor chain (target_layer=L{}) failed",
            anchor_layer
        )
    })?;
    let num_active = chain_result.active_links.len();
    if num_active > MAX_CHAIN_LEN {
        bail!(
            "internal: {} active chain links exceed MAX_CHAIN_LEN={} (anchor_layer=L{}, \
             event_seq={}, W={}, P={}, anchor_kb={})",
            num_active,
            MAX_CHAIN_LEN,
            anchor_layer,
            event_seq,
            w,
            p,
            chain_result.anchor_kb_seqno,
        );
    }
    info!(
        "chain built: anchor_layer=L{}, anchor_kb={}, active_links={}, final_root={}",
        anchor_layer,
        chain_result.anchor_kb_seqno,
        num_active,
        hex::encode(chain_result.final_chain_root),
    );

    // Look up the verifier's mirrored layer hash.
    let anchor_key_block_seq = chain_result.anchor_kb_seqno;
    let key_block_height = fetch_block_observed_height(gql, anchor_key_block_seq)
        .await
        .with_context(|| {
            format!(
                "fetching observed_height for anchor key block {anchor_key_block_seq} \
                 (target_layer=L{})",
                anchor_layer,
            )
        })?;
    info!(
        "anchor key block {} observed_height = {}",
        anchor_key_block_seq, key_block_height,
    );

    let target_layer_idx0 = (anchor_layer - 1) as usize;
    let slot = bridge_state
        .slot_for_event_height(anchor_layer, key_block_height)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "anchor key block height {} not found in L{} window {:?} — block has rolled out \
                 of the L{} rolling window. Re-run with `--anchor-layer auto` (probes L1 then L2) \
                 or explicitly pick a higher layer if the state has progressed to it.",
                key_block_height,
                anchor_layer,
                bridge_state.layer_windows[target_layer_idx0]
                    .iter_chronological()
                    .map(|(_, h)| h)
                    .collect::<Vec<_>>(),
                anchor_layer,
            )
        })?;
    info!("L{} slot for this anchor key block: {}", anchor_layer, slot,);

    let chosen_layer_hash = bridge_state.layer_windows[target_layer_idx0]
        .iter_chronological()
        .nth(slot)
        .map(|(h, _)| h)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: L{} slot {} found but not present when iterating chronologically",
                anchor_layer,
                slot,
            )
        })?;

    if chosen_layer_hash != chain_result.final_chain_root {
        warn!(
            "final chain root mismatch — verifier mirror (L{} root @ anchor KB) = {}, locally \
             rebuilt = {}. The proof will not satisfy the circuit until every tree along the \
             chain matches the node's construction byte-for-byte.",
            anchor_layer,
            hex::encode(chosen_layer_hash),
            hex::encode(chain_result.final_chain_root),
        );
    }

    // Assemble MAX_CHAIN_LEN links.
    let inactive_depth = block_tree_proof.siblings_hex.len();
    let mut dense_chain_native: Vec<DenseChainLink> = Vec::with_capacity(MAX_CHAIN_LEN);
    dense_chain_native.extend(chain_result.active_links);
    for _ in num_active..MAX_CHAIN_LEN {
        dense_chain_native.push(DenseChainLink::inactive(chosen_layer_hash, inactive_depth));
    }
    debug_assert_eq!(dense_chain_native.len(), MAX_CHAIN_LEN);
    let dense_chain_ser: Vec<DenseChainLinkSer> = dense_chain_native
        .iter()
        .map(|link| DenseChainLinkSer {
            active: link.active,
            position: link.position as u32,
            siblings_hex: link.siblings.iter().map(hex::encode).collect(),
            leaf_hex: hex::encode(link.leaf_native),
        })
        .collect();

    let anchor = AnchorRef {
        layer_idx: (anchor_layer - 1) as u32,
        height: key_block_height,
        layer_hash_hex: hex::encode(chosen_layer_hash),
        dense_chain: dense_chain_ser,
        num_active_chain_steps: num_active as u32,
    };

    let events_tree_depth = events_tree_proof.siblings_hex.len();
    let block_tree_depth = block_tree_proof.siblings_hex.len();
    let layer_hash_hex = anchor.layer_hash_hex.clone();

    // h07_sibling_hex — the depth-4 SHA-256 block-id tree's h0_7 aggregate,
    // consumed by the multi-thread `BridgeEventFinalProof` circuit's L8
    // opening (schema.rs:71). Derive from the event block's canonical
    // `block_merkle_tree_leaves` (served by the node since acki-nacki
    // 2608f686e); the exporter cannot know this because it has only the
    // ExtOut BOC. Older blocks without `block_merkle_tree_leaves` fail
    // fast rather than silently producing a garbage witness.
    let event_block = gql
        .query_proof_block_by_seqno(event_seq)
        .await
        .with_context(|| {
            format!("fetching event block seq={event_seq} for h07_sibling derivation")
        })?;
    let leaves = event_block.block_merkle_tree_leaves.ok_or_else(|| {
        anyhow::anyhow!(
            "event block seq={} does not expose block_merkle_tree_leaves — node predates the \
             depth-4 block-id tree exposure (acki-nacki commit 2608f686e). Cross-thread / \
             multi-hop event proofs are unsupported against this node.",
            event_seq,
        )
    })?;
    let h07_sibling = BlockIdMerkleTree::from_leaves(leaves).h0_7;
    partial.h07_sibling_hex = hex::encode(h07_sibling);
    info!(
        "h07_sibling derived from block_merkle_tree_leaves: {}",
        partial.h07_sibling_hex,
    );

    partial.events_tree_proof = Some(events_tree_proof);
    partial.block_tree_proof = Some(block_tree_proof);
    partial.anchor = Some(anchor);

    let summary = EnrichSummary {
        schema_version: partial.schema_version,
        event_message_hash_hex: partial.event_message_hash_hex.clone(),
        block_seq_no: partial.block_seq_no,
        key_block_seq_no: key_block_seq,
        thinned_key_block_seq_no: anchor_key_block_seq,
        layer_idx: (anchor_layer - 1) as u32,
        layer_hash_hex,
        events_tree_depth,
        block_tree_depth,
        num_active_chain_steps: num_active as u32,
        auto_escalated: escalated_from_auto,
    };

    Ok(EnrichedWitness {
        witness: partial,
        summary,
    })
}

// ============================================================================
// Helpers — internal to enrichment. Kept `pub(crate)` so tests could reach
// them if needed but not part of the stable library surface.
// ============================================================================

/// Resolve the requested [`AnchorLayerMode`] into a concrete 1-indexed
/// layer number (1 = L1, 2 = L2, …, up to `MAX_LAYERS`).
///
/// * `Explicit(n)` — returns `n` unchanged. Rollout detection is left to the
///   downstream `slot_for_event_height` lookup so the error carries the exact
///   window contents.
/// * `Auto` — probes the verifier state, cheapest layer first:
///   1. Compute `K` via [`real_chain_builder::l1_anchor_boundaries`] and fetch
///      its observed_height. If `K`'s height is in `layer_windows[0]`, pick L1
///      (cheapest wait budget).
///   2. Otherwise, loop `n = 2..=num_active_layers` (capped at `MAX_LAYERS`).
///      For each `n`, compute `T_n` via
///      [`real_chain_builder::l_n_anchor_boundaries`], fetch its
///      observed_height, and check `layer_windows[n-1]`. First match wins.
///   3. Otherwise fail — event is older than the verifier's coverage.
///
/// Returns `(anchor_layer, escalated_from_auto)`.
pub(crate) async fn resolve_anchor_layer(
    gql: &GqlClient,
    bridge_state: &BridgeState,
    event_seq: u64,
    mode: AnchorLayerMode,
) -> Result<(u8, bool)> {
    if let AnchorLayerMode::Explicit(n) = mode {
        return Ok((n, false));
    }

    let w = HISTORY_WINDOW_SIZE;
    let p = THINNING_FACTOR_P;

    // Probe L1.
    let (_h_e, k, _hops) = real_chain_builder::l1_anchor_boundaries(event_seq, w, p);
    let k_height = fetch_block_observed_height(gql, k)
        .await
        .with_context(|| format!("auto-probe: fetching observed_height for L1 anchor K={k}"))?;
    if bridge_state.slot_for_event_height(1, k_height).is_some() {
        info!(
            "auto: L1 slot present for K={} (height={}); using L1",
            k, k_height
        );
        return Ok((1, false));
    }
    info!(
        "auto: L1 slot for K={} (height={}) rolled out of L1 window — probing L2+",
        k, k_height,
    );

    // Escalate L2 → … → L(min(num_active_layers, MAX_LAYERS)).
    let num_active = bridge_state.num_active_layers() as u8;
    if num_active < 2 {
        bail!(
            "auto: L1 anchor K={} (height={}) rolled out of the L1 rolling window and the \
             verifier state has only {} active layer(s) — no higher layer to escalate to. Event \
             is older than L1 coverage; L(N≥2) escalation requires the verifier to have observed \
             at least one L(N) boundary.",
            k,
            k_height,
            num_active,
        );
    }
    let max_probe = num_active.min(MAX_LAYERS as u8);
    let mut probes: Vec<(u8, u64, u64)> = Vec::with_capacity((max_probe - 1) as usize);
    for n in 2..=max_probe {
        let boundaries = real_chain_builder::l_n_anchor_boundaries(event_seq, w, n);
        let t_n = *boundaries
            .last()
            .expect("l_n_anchor_boundaries returns ≥ 1 entry");
        let t_n_height = fetch_block_observed_height(gql, t_n)
            .await
            .with_context(|| {
                format!("auto-probe: fetching observed_height for L{n} anchor T_{n}={t_n}")
            })?;
        if bridge_state.slot_for_event_height(n, t_n_height).is_some() {
            info!(
                "auto: escalating L1 → L{} for event_seq={} (K={} rolled out; T_{}={} in L{} \
                 window at height={})",
                n, event_seq, k, n, t_n, n, t_n_height,
            );
            return Ok((n, true));
        }
        info!(
            "auto: L{} slot for T_{}={} (height={}) also rolled out — probing next layer",
            n, n, t_n, t_n_height,
        );
        probes.push((n, t_n, t_n_height));
    }

    let probes_str = probes
        .iter()
        .map(|(n, t, h)| format!("L{n}: T_{n}={t}(height={h})"))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "auto: L1 (K={}, height={}) and all higher active layers rolled out of their rolling \
         windows. Event is older than verifier coverage. Probed: {}. num_active_layers={}, \
         MAX_LAYERS={}.",
        k,
        k_height,
        probes_str,
        num_active,
        MAX_LAYERS,
    );
}

/// Empirical verifier catch-up rate on shellnet — see notes in the
/// original `bin/build.rs`. Used only for the human-readable wait
/// estimate emitted by [`guard_wait_time`].
const VERIFIER_SECS_PER_SEQNO: f64 = 0.5;

/// Wait-time guard threshold: worst-case verifier catch-up above this
/// requires opt-in.
const WAIT_TIME_WARN_THRESHOLD_SECS: f64 = 15.0 * 60.0;

/// Return the worst-case verifier catch-up window (in seq_no units) for
/// the chosen anchor layer.
pub(crate) fn worst_case_wait_blocks(anchor_layer: u8, w: u64, p: u64) -> u64 {
    match anchor_layer {
        1 => w * p - 1,
        n => w.saturating_pow(n as u32).saturating_sub(1),
    }
}

/// Warn about the wait time for the chosen anchor layer; error out if
/// the worst case exceeds [`WAIT_TIME_WARN_THRESHOLD_SECS`] and the
/// caller has not passed `i_know_the_wait = true`.
pub(crate) fn guard_wait_time(anchor_layer: u8, i_know_the_wait: bool, w: u64) -> Result<()> {
    let p = THINNING_FACTOR_P;
    let max_blocks = worst_case_wait_blocks(anchor_layer, w, p);
    let max_secs = (max_blocks as f64) * VERIFIER_SECS_PER_SEQNO;

    if max_secs <= WAIT_TIME_WARN_THRESHOLD_SECS {
        return Ok(());
    }

    let max_minutes = max_secs / 60.0;
    if !i_know_the_wait {
        bail!(
            "--anchor-layer {} has an impractical wait budget: worst case {} blocks (≈ {:.0} min \
             at ~{:.1}s per seq_no verifier catch-up). Re-run with --i-know-the-wait if this is \
             intentional; typical E2E callers should stick with --anchor-layer 1 (L1, ≤ W·P−1 \
             blocks ≈ 4 min).",
            anchor_layer,
            max_blocks,
            max_minutes,
            VERIFIER_SECS_PER_SEQNO,
        );
    }
    warn!(
        "anchor_layer=L{}: worst-case verifier catch-up ≈ {} blocks (~{:.0} min); proceeding \
         because --i-know-the-wait was passed",
        anchor_layer, max_blocks, max_minutes,
    );
    Ok(())
}

pub(crate) fn parse_hex32(label: &str, s: &str) -> Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).with_context(|| format!("{label}: invalid hex"))?;
    if bytes.len() != 32 {
        bail!("{label}: expected 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Fetch the event's block envelope, rebuild the same Poseidon dense Merkle
/// tree the node uses for `tracked_ext_out_messages`, locate the leaf for
/// this event, and return its proof in schema form.
pub(crate) async fn build_events_tree_proof(
    gql: &GqlClient,
    event_seq: u64,
    dapp: &[u8; 32],
    acc: &[u8; 32],
    event_repr_hash: &[u8; 32],
) -> Result<MerkleProofData> {
    use bridge_prover_lib::poseidon_dense::{
        compute_ext_message_leaf_hash, dense_merkle_proof, PoseidonHasher,
    };

    let block = gql
        .query_proof_block_by_seqno(event_seq)
        .await
        .with_context(|| format!("fetching proof block for event block seq={event_seq}"))?;

    let mut leaves: Vec<[u8; 32]> = Vec::new();
    for (account_routing, messages) in block.tracked_ext_out_messages.iter() {
        let (route_dapp, route_acc) = account_routing.unpack_for_hash();
        for msg in messages {
            leaves.push(compute_ext_message_leaf_hash(&route_dapp, &route_acc, msg));
        }
    }
    if leaves.is_empty() {
        bail!(
            "block seq={event_seq} has no tracked_ext_out_messages — cannot have emitted a \
             WithdrawalInitiated event there"
        );
    }

    let target_leaf = compute_ext_message_leaf_hash(dapp, acc, event_repr_hash);
    let position = leaves
        .iter()
        .position(|l| *l == target_leaf)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "target event leaf {} not found in block seq={}'s tracked_ext_out_messages",
                hex::encode(target_leaf),
                event_seq,
            )
        })?;

    let hasher = PoseidonHasher::new();
    let siblings = dense_merkle_proof(&hasher, &leaves, position);

    Ok(MerkleProofData {
        position: position as u32,
        siblings_hex: siblings.iter().map(hex::encode).collect(),
    })
}

/// Build the L1 history-window tree for the given key block and return the
/// Merkle proof for the block at `block_offset_in_window`, together with
/// the L1 root we computed.
pub(crate) async fn build_block_tree_proof(
    gql: &GqlClient,
    key_block_seq: u64,
    w: u64,
    block_offset_in_window: u64,
) -> Result<(MerkleProofData, [u8; 32])> {
    let l2_step = w * w;
    let most_recent_l2_block = (key_block_seq / l2_step) * l2_step;
    let higher_layer_root: [u8; 32] = if most_recent_l2_block == 0 {
        [0u8; 32]
    } else {
        match gql.query_proof_block_by_seqno(most_recent_l2_block).await {
            Ok(b) => b.history_proofs.get(&2u8).copied().unwrap_or([0u8; 32]),
            Err(e) => {
                warn!(
                    "failed to fetch L2 root from most-recent-L2 key block {}: {} — using zero",
                    most_recent_l2_block, e
                );
                [0u8; 32]
            },
        }
    };

    let prev_key_block_seq = key_block_seq.saturating_sub(w);
    let prev_same_layer_root: [u8; 32] = if prev_key_block_seq == 0 {
        [0u8; 32]
    } else {
        match gql.query_proof_block_by_seqno(prev_key_block_seq).await {
            Ok(b) => b.history_proofs.get(&1u8).copied().unwrap_or([0u8; 32]),
            Err(e) => {
                warn!(
                    "failed to fetch previous L1 root from key block {}: {} — using zero",
                    prev_key_block_seq, e
                );
                [0u8; 32]
            },
        }
    };

    let window_start = key_block_seq - w;
    let mut block_leaves = Vec::with_capacity(w as usize);
    for seq in window_start..key_block_seq {
        let b = gql
            .query_proof_block_by_seqno(seq)
            .await
            .with_context(|| format!("fetching block seq={seq} in L1 window"))?;
        block_leaves.push(bridge_prover_lib::poseidon_dense::compute_block_leaf_hash(
            &b.block_id,
            &b.envelope_hash,
            &b.tracked_ext_out_messages_root,
        ));
    }

    let mut leaves = Vec::with_capacity(2 + block_leaves.len() + 2);
    leaves.push(higher_layer_root);
    leaves.push(prev_same_layer_root);
    leaves.extend_from_slice(&block_leaves);
    pad_leaves_to_power_of_2(&mut leaves);

    let leaf_position = (2 + block_offset_in_window) as usize;
    if leaf_position >= leaves.len() {
        bail!(
            "internal: leaf_position {} >= leaves.len() {} (W={}, offset={})",
            leaf_position,
            leaves.len(),
            w,
            block_offset_in_window,
        );
    }

    let (root, siblings) = build_tree_and_proof(&leaves, leaf_position);
    Ok((
        MerkleProofData {
            position: leaf_position as u32,
            siblings_hex: siblings.iter().map(hex::encode).collect(),
        },
        root,
    ))
}

/// Resolve a block's `observed_height` (=
/// `common_section.block_height.height()`).
pub(crate) async fn fetch_block_observed_height(gql: &GqlClient, seq: u64) -> Result<u64> {
    let block = gql.query_proof_block_by_seqno(seq).await?;
    Ok(block.height)
}

/// Detect whether the event's block is on the same thread as the anchor
/// (currently `DEFAULT_THREAD_ID` = thread 0 for the bridge cut) and, if
/// cross-thread, walk `proof_block_refs` back to a same-thread ancestor —
/// emitting a `MultiHopBundleWitnessJson` the multi-hop verifier consumes.
///
/// Returns `MultiHopBundleWitnessJson::default()` (i.e. `snarks = []`) for
/// same-thread claims — the signal the prover uses to short-circuit
/// `y_block_id = x_block_id`.
///
/// # Cross-thread walk (Direction (a))
///
/// Starting from `event_block` (on some non-default thread), the walker
/// repeatedly picks a `proof_block_refs[i]` with `i >= 1` that leads to a
/// strictly different thread, preferring the default thread when it appears
/// among the candidates. Slot 0 (`parent_block_id`) is a valid hop edge
/// from the circuit's point of view but points at the same thread as the
/// current block, so it cannot progress a cross-thread walk and is skipped
/// here. Because `refs` on Acki Nacki only ever point at strictly older
/// blocks, the walk emits snarks in newest→oldest order — matching
/// Direction (a) of the cross-thread reachability model (event `X` newer on
/// thread `t`; anchor `Y` older on thread 0). Each step produces one
/// `MultiHopProofWitnessJson` snark carrying:
///
/// * the SHA-256 depth-4 L7 opening of `block_merkle_tree_leaves[7]`
///   (`proof_block_refs_root`) against the current block's `block_id`
///   (`block_merkle_leaf_proof` — mirrors `gql_proof.rs::block_merkle_leaf_proof`);
/// * the Poseidon dense-merkle inner path of the chosen
///   `proof_block_refs[ref_index]` against L7
///   (`proof_block_ref_inner_path_native` — mirrors
///   `history-proof::dense_merkle_proof` with the byte-flat sponge convention);
/// * the clear-byte endpoints — Direction (a):
///   `hop_start_block_id = block_id` (the *current* block being opened,
///   newer) and `hop_end_block_id = refs[ref_index]` (the *older* ref
///   extracted from that block).
///
/// Both openings are re-verified natively before the witness is shipped —
/// a mismatched leaf or wrong `refs_tree_depth` fails here instead of
/// blowing up in-circuit.
///
/// The walk terminates when the next referenced block is on the default
/// thread. The emitted `snarks` are already in newest→oldest order — no
/// reverse — so `snarks[0].hops[0].hop_start` = event block (`X`) and
/// `snarks[K-1].hops.last().hop_end` = default-thread ancestor (`Y`). The
/// circuit's `x_block_id` / `y_block_id` public inputs bind to those two
/// endpoints.
///
/// # Errors
///
/// * `event_block` on a non-default thread with `proof_block_refs.len() < 2`
///   — no cross-thread ref to hop through.
/// * No ref leads to a different thread (walk is stuck).
/// * More than `N_BUNDLE_MAX = 20` hops needed — over the prototype cap.
/// * A referenced block is missing `block_merkle_tree_leaves` (very old
///   blocks that predate the node exposing the field).
/// * A native re-check of the L7 SHA-256 or Poseidon opening fails —
///   chain-side data inconsistency that the prover would surface downstream.
pub async fn resolve_cross_thread_chain(
    gql: &GqlClient,
    event_seq: u64,
) -> Result<MultiHopBundleWitnessJson> {
    let event_block = gql
        .query_proof_block_by_seqno(event_seq)
        .await
        .with_context(|| format!("fetching event block seq={event_seq} for thread detection"))?;

    // Compare thread_id via its canonical 34-byte identity. The default
    // (all-zero) thread is what the bridge anchors to today.
    let default_thread = ThreadIdentifier::default();
    if event_block.thread_id == default_thread {
        info!(
            "event block seq={} is on the default thread — same-thread claim, empty bundle",
            event_seq,
        );
        return Ok(MultiHopBundleWitnessJson::default());
    }

    info!(
        event_seq,
        thread = %event_block.thread_id,
        "event block is on a non-default thread — walking proof_block_refs back to thread 0",
    );

    let mut snarks: Vec<MultiHopProofWitnessJson> = Vec::new();
    let mut cur_block = event_block;

    loop {
        let (ref_index, next_block) = pick_next_hop(gql, &cur_block, &default_thread)
            .await
            .with_context(|| {
                format!(
                    "picking next hop from block {} on thread {}",
                    hex::encode(cur_block.block_id),
                    cur_block.thread_id
                )
            })?;

        let hop = build_hop_witness(&cur_block, ref_index)?;
        snarks.push(MultiHopProofWitnessJson {
            hops: [hop],
        });

        if next_block.thread_id == default_thread {
            info!(
                hops = snarks.len(),
                terminal_block = %hex::encode(next_block.block_id),
                "L7 walk reached the default thread",
            );
            break;
        }
        if snarks.len() >= N_BUNDLE_MAX {
            bail!(
                "cross-thread chain from event seq={} exceeds N_BUNDLE_MAX = {} hops \
                 without reaching the default thread",
                event_seq,
                N_BUNDLE_MAX
            );
        }
        cur_block = next_block;
    }

    // Direction (a): snarks are already newest→oldest. snarks[0].hop_start =
    // event block (== x_block_id), snarks[K-1].hop_end = default-thread
    // ancestor (== y_block_id). Cross-hop continuity in the circuit is
    // `snarks[i].hop_end == snarks[i+1].hop_start` — the older ref extracted
    // by hop i is the current block opened by hop i+1.
    Ok(MultiHopBundleWitnessJson {
        snarks,
    })
}

/// Pick the next hop from `cur`. Scans `cur.proof_block_refs[1..]` in order,
/// fetches each referenced block, and returns:
///
/// * the first ref whose block is on `default_thread` (preferred terminus), or
/// * the first ref whose block is on a strictly different thread from `cur`
///   (progresses the walk), or
/// * an error if no such ref exists.
///
/// Slot 0 (`parent_block_id`) is a valid hop edge from the circuit's point
/// of view, but it points at the same thread as `cur` by construction and
/// therefore cannot provide cross-thread progress — this walker skips it as
/// a cross-thread-specific performance choice (saves one GQL round-trip per
/// block). Future walkers that need same-thread hops may call
/// `build_hop_witness(cur, 0)` directly.
async fn pick_next_hop(
    gql: &GqlClient,
    cur: &GqlProofBlock,
    default_thread: &ThreadIdentifier,
) -> Result<(u32, GqlProofBlock)> {
    if cur.proof_block_refs.len() < 2 {
        bail!(
            "block {} on thread {} has no cross-thread refs \
             (proof_block_refs.len() = {}); cannot hop",
            hex::encode(cur.block_id),
            cur.thread_id,
            cur.proof_block_refs.len()
        );
    }

    let mut fallback: Option<(u32, GqlProofBlock)> = None;
    for (i, ref_id) in cur.proof_block_refs.iter().enumerate().skip(1) {
        let ref_hex = hex::encode(ref_id);
        let ref_block = gql
            .query_proof_block_by_id(&ref_hex)
            .await
            .with_context(|| {
                format!(
                    "fetch ref block {ref_hex} (slot {i} of block {})",
                    hex::encode(cur.block_id)
                )
            })?;
        if ref_block.thread_id == *default_thread {
            return Ok((i as u32, ref_block));
        }
        if ref_block.thread_id != cur.thread_id && fallback.is_none() {
            fallback = Some((i as u32, ref_block));
        }
    }
    fallback.ok_or_else(|| {
        anyhow::anyhow!(
            "block {} on thread {} has no ref leading to a different thread — L7 walk stuck \
             ({} refs scanned)",
            hex::encode(cur.block_id),
            cur.thread_id,
            cur.proof_block_refs.len() - 1,
        )
    })
}

/// Build one `HopWitnessJson` opening `cur.proof_block_refs[ref_index]`
/// against `cur.block_id` via the L7 SHA-256 leaf proof + Poseidon dense
/// merkle inner path. Both openings are re-verified natively so a
/// malformed chain payload fails here rather than in-circuit.
fn build_hop_witness(cur: &GqlProofBlock, ref_index: u32) -> Result<HopWitnessJson> {
    let leaves = cur.block_merkle_tree_leaves.ok_or_else(|| {
        anyhow::anyhow!(
            "block {} missing block_merkle_tree_leaves — cannot build hop witness",
            hex::encode(cur.block_id)
        )
    })?;

    let idx = ref_index as usize;
    if idx >= cur.proof_block_refs.len() {
        bail!(
            "ref_index {} out of range (proof_block_refs.len() = {})",
            idx,
            cur.proof_block_refs.len()
        );
    }

    let l7_sibling_path = block_merkle_leaf_proof(&leaves, 7);
    let (inner_path, refs_tree_depth) =
        proof_block_ref_inner_path_native(&cur.proof_block_refs, idx);

    // Defense-in-depth: re-verify both openings before shipping the witness.
    let l7_leaf = leaves[7];
    if !verify_block_merkle_leaf_proof(&cur.block_id, &l7_leaf, 7, &l7_sibling_path) {
        bail!(
            "block {}: L7 SHA-256 opening does not verify against block_id — \
             chain data inconsistent",
            hex::encode(cur.block_id)
        );
    }
    let ref_leaf = ref_leaf_hash_native(idx, &cur.proof_block_refs[idx]);
    if !verify_proof_block_ref_inner_path(&l7_leaf, &ref_leaf, idx, &inner_path, refs_tree_depth) {
        bail!(
            "block {}: L7 Poseidon opening at ref_index {} does not verify — \
             chain data inconsistent",
            hex::encode(cur.block_id),
            idx
        );
    }

    // Direction (a): start = current (newer) block; end = older ref.
    let hop_start_block_id = cur.block_id;
    let hop_end_block_id = cur.proof_block_refs[idx];

    let block_json = BlockWitnessJson {
        block_id_hex: hex::encode(cur.block_id),
        block_merkle_tree_leaves_hex: std::array::from_fn(|i| hex::encode(leaves[i])),
        proof_block_refs_hex: cur.proof_block_refs.iter().map(hex::encode).collect(),
    };

    Ok(HopWitnessJson {
        is_active: true,
        block: block_json,
        block_merkle_leaf_proof_l7_hex: std::array::from_fn(|i| hex::encode(l7_sibling_path[i])),
        ref_index,
        refs_tree_depth,
        proof_block_ref_inner_path_hex: std::array::from_fn(|i| hex::encode(inner_path[i])),
        hop_start_block_id_hex: hex::encode(hop_start_block_id),
        hop_end_block_id_hex: hex::encode(hop_end_block_id),
    })
}

#[cfg(test)]
mod cross_thread_tests {
    //! Pure-Rust tests for `build_hop_witness`. The GQL-driven
    //! `resolve_cross_thread_chain` needs a live `GqlClient`, so it is
    //! covered by the daemon integration tests (see
    //! `bridge-relayer-daemon/src/withdraw_e2e`) — here we only pin the
    //! byte-flat witness assembly against the same primitives the circuit
    //! uses.
    use bridge_event_prove_circuit::multi_hop_witness::{
        block_merkle_root, proof_block_refs_root_native, BLOCK_MERKLE_LEAF_COUNT,
        MAX_PROOF_BLOCK_REFS_DEPTH,
    };
    use bridge_gql_fetcher::gql_client::GqlProofBlock;
    use bridge_gql_fetcher::types::ThreadIdentifier;
    use std::collections::BTreeMap;

    use super::build_hop_witness;

    /// Assemble a synthetic `GqlProofBlock` whose L7 leaf equals the
    /// Poseidon root over `refs` and whose `block_id` equals the SHA-256
    /// root over its own leaves — the invariant the walker's re-check
    /// asserts.
    fn synth_block(thread_seed: u8, refs: Vec<[u8; 32]>) -> GqlProofBlock {
        let mut leaves = [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT];
        // Fill L0..L6, L8..L15 with distinguishable garbage so the L7 slot
        // is what the opening actually binds to.
        for (i, leaf) in leaves.iter_mut().enumerate() {
            leaf[0] = thread_seed;
            leaf[1] = i as u8;
        }
        leaves[7] = proof_block_refs_root_native(&refs);
        let block_id = block_merkle_root(&leaves);

        // 34-byte thread_id: seed in the first byte, rest zero. The
        // canonical default is all-zero; any non-zero seed produces a
        // distinct thread.
        let mut tid_bytes = [0u8; 34];
        tid_bytes[0] = thread_seed;
        let thread_id_hex = hex::encode(tid_bytes);
        let thread_id = ThreadIdentifier::try_from(thread_id_hex).expect("thread_id");

        GqlProofBlock {
            id: format!("synth-{thread_seed}"),
            block_id,
            thread_id,
            height: thread_seed as u64,
            envelope_hash: [0u8; 32],
            tracked_ext_out_messages_root: [0u8; 32],
            tracked_ext_out_messages: BTreeMap::new(),
            history_proofs: BTreeMap::new(),
            proof_block_refs: refs,
            block_merkle_tree_leaves: Some(leaves),
        }
    }

    #[test]
    fn hop_witness_roundtrips_with_native_openings() {
        // 3 refs: slot 0 = parent (valid hop edge, exercised by
        // `hop_witness_accepts_ref_index_zero` below); slots 1, 2 =
        // cross-thread candidates.
        let refs = vec![[0xAAu8; 32], [0xBBu8; 32], [0xCCu8; 32]];
        let block = synth_block(1, refs.clone());
        let hop = build_hop_witness(&block, 2).expect("hop witness");

        assert!(hop.is_active);
        assert_eq!(hop.ref_index, 2);
        // Direction (a): start = current (newer) block; end = older ref.
        assert_eq!(hop.hop_start_block_id_hex, hex::encode(block.block_id));
        assert_eq!(hop.hop_end_block_id_hex, hex::encode(refs[2]));
        // refs.len() = 3 → next_power_of_two = 4 → depth = 2.
        assert_eq!(hop.refs_tree_depth, 2);
        // Padded siblings beyond refs_tree_depth are zero.
        for i in (hop.refs_tree_depth as usize)..MAX_PROOF_BLOCK_REFS_DEPTH {
            assert_eq!(hop.proof_block_ref_inner_path_hex[i], hex::encode([0u8; 32]));
        }
    }

    #[test]
    fn hop_witness_accepts_ref_index_zero() {
        // Slot 0 is `parent_block_id` and is a valid hop edge — the native
        // re-check inside `build_hop_witness` must succeed and the resulting
        // witness must bind to the parent block at `hop_end_block_id`.
        let refs = vec![[0xAAu8; 32], [0xBBu8; 32]];
        let block = synth_block(1, refs.clone());
        let hop = build_hop_witness(&block, 0).expect("slot-0 hop witness");

        assert!(hop.is_active);
        assert_eq!(hop.ref_index, 0);
        assert_eq!(hop.hop_start_block_id_hex, hex::encode(block.block_id));
        assert_eq!(hop.hop_end_block_id_hex, hex::encode(refs[0]));
    }

    #[test]
    fn hop_witness_rejects_out_of_range_ref_index() {
        let refs = vec![[0xAAu8; 32], [0xBBu8; 32]];
        let block = synth_block(1, refs);
        let err = build_hop_witness(&block, 5).expect_err("must reject overflow");
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn hop_witness_rejects_missing_block_merkle_leaves() {
        let refs = vec![[0xAAu8; 32], [0xBBu8; 32]];
        let mut block = synth_block(1, refs);
        block.block_merkle_tree_leaves = None;
        let err = build_hop_witness(&block, 1).expect_err("must reject missing leaves");
        assert!(err.to_string().contains("missing block_merkle_tree_leaves"));
    }

    #[test]
    fn hop_witness_detects_l7_inconsistency() {
        // Tamper with the L7 leaf so the Poseidon opening no longer matches.
        let refs = vec![[0xAAu8; 32], [0xBBu8; 32]];
        let mut block = synth_block(1, refs);
        let mut leaves = block.block_merkle_tree_leaves.unwrap();
        leaves[7][0] ^= 0xFF;
        // Re-derive block_id so the SHA-256 opening still verifies —
        // this isolates the Poseidon-side failure.
        block.block_id = block_merkle_root(&leaves);
        block.block_merkle_tree_leaves = Some(leaves);
        let err = build_hop_witness(&block, 1).expect_err("must reject inconsistent L7");
        assert!(err.to_string().contains("Poseidon opening"));
    }
}
