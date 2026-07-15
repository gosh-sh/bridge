//! `bridge-event-witness-builder` — Track D1 of the Circuit 4 integration plan.
//!
//! Reads a *partial* `PrivateWitness` JSON (the one produced by
//! `bridge-event-private-witness-export` — see Track B), pulls the
//! daemon-side data it needs from GraphQL + the bridge state file, and
//! writes an *enriched* `PrivateWitness` JSON that `bridge-event-prove
//! --fixture` (Track D3) can consume to generate a real Circuit 4 proof.
//!
//! ### What the per-tx exporter cannot fill in
//!
//!   * `events_tree_proof` — Poseidon Merkle proof from this event's
//!     `ext_msg_leaf` (= `Poseidon96(dapp || account || repr_hash)`) up to
//!     the block's `ext_out_messages_root`. Built from the same
//!     `tracked_ext_out_messages` map the node uses (see
//!     `history_proof::dense_merkle_proof` in
//!     `acki-nacki/node/libs/history-proof/src/lib.rs`).
//!   * `block_tree_proof` — Poseidon Merkle proof from this block's
//!     `block_leaf` (= `Poseidon96(block_id || envelope_hash || ext_out_root)`)
//!     up to `root_1` (the L1 history-window root the verifier mirrors).
//!     Built with the production L1 tree shape: 8 leaves
//!     `[higher_layer_root, prev_same_layer_root, block_leaf_0, ...,
//!     block_leaf_{W-1}]` padded to the next power of 2 (16 leaves at W=8,
//!     so 4-deep proofs). Same layout `bridge-prover-lib::real_chain_builder
//!     ::build_layer1_tree` uses.
//!   * `anchor` — references the L1 layer hash the verifier has mirrored
//!     for the key block containing this event. Carries the chosen layer
//!     hash (the value the circuit publishes as `PUB_FINAL_ROOT`) and the
//!     `dense_chain` (here 0 active steps, just inactive padding to
//!     `MAX_CHAIN_LEN` — the L1 root *is* the chosen layer hash, so no
//!     hops up to a higher layer are needed).
//!
//! ### L1-only anchor; L1→L5 escalation deferred
//!
//! This first cut handles **only** L1 anchoring. If the event's block has
//! rolled out of the L1 rolling window (i.e. no slot in
//! `state.layer_windows[0]` matches the key block's observed_height), the
//! command fails with a clear error rather than silently producing an
//! un-anchored witness.
//!
//! TODO(L1→L5 escalation): when no L1 slot matches, walk up: look for the
//! event's parent L2 key block in `state.layer_windows[1]`, then L3, …
//! Each escalation step adds one active `dense_chain` link bridging the
//! lower-layer root to the chosen higher layer's root. The chain must
//! follow the production `real_chain_builder::build_layer_n_tree` shape so
//! the recomputed `final_root` matches the verifier's mirrored layer hash.
//!
//! ### Mode and exit codes
//!
//! ```text
//! bridge-event-witness-builder
//!   --partial-witness  <path>                  (required)
//!   --state            ./state/prover_state.json
//!   --gql-endpoint     http://localhost/graphql
//!   --layer-idx        0                       (L1 only for now)
//!   --out              <path>                  (required)
//! ```
//!
//! Exit code 0 on success, non-zero on any failure. A one-line JSON
//! summary is printed to stdout (dex-tooling style — the last non-empty
//! line is the result), logging goes to stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tracing::{error, info, warn};

use bridge_prover_lib::bridge_state::BridgeState;
use bridge_prover_lib::chain_proof_builder::{
    build_tree_and_proof, pad_leaves_to_power_of_2,
};
use bridge_prover_lib::gql_client::{self, GqlClient};

use bridge_event_witness::schema::{
    AnchorRef, DenseChainLinkSer, MerkleProofData, PrivateWitness, SCHEMA_VERSION,
};

use gosh_dense_balanced_tree::{DenseChainLink, MAX_CHAIN_LEN};

// Pull the same canonical constants the rest of the daemon stack uses, so
// the witness builder cannot drift from the verifier.
const HISTORY_WINDOW_SIZE: u64 =
    bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64;

/// Prover-side thinning factor `P`. Re-exported from `bridge_prover_lib`
/// so the witness builder uses the same value as the prover daemon.
const THINNING_FACTOR_P: u64 = bridge_prover_lib::THINNING_FACTOR_P;

/// Default path of the bridge-state JSON file the witness builder reads.
///
/// We default to `prover_state.json` because the standalone CI orchestrator
/// (`bridge_e2e_self_contained.py`) runs without a verifier daemon and only
/// has the prover's state on disk. The paired-mode orchestrator
/// (`generate_withdrawals_with_live_event_proving.py`) overrides this with
/// `--state ./state/verifier_state.json` — the two files store the same
/// information for the fields the witness builder consumes
/// (`layer_windows`, `heights`, `stored_last_seen_*`, `initialized`),
/// so picking one over the other is a question of which daemon is the
/// authority in the running configuration, not of content.
const DEFAULT_STATE: &str = "./state/prover_state.json";
const DEFAULT_GQL_ENDPOINT: &str = "http://localhost/graphql";

/// Top-level CLI args.
#[derive(Debug)]
struct CliArgs {
    partial_witness: PathBuf,
    bridge_state: PathBuf,
    gql_endpoint: String,
    layer_idx: u32,
    out: PathBuf,
}

impl CliArgs {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut partial_witness: Option<PathBuf> = None;
        let mut bridge_state: Option<PathBuf> = None;
        let mut gql_endpoint: Option<String> = None;
        let mut layer_idx: u32 = 0;
        let mut out: Option<PathBuf> = None;

        while let Some(a) = args.next() {
            match a.as_str() {
                "--partial-witness" => {
                    let v = args.next().context("--partial-witness needs a path")?;
                    partial_witness = Some(PathBuf::from(v));
                }
                "--state" => {
                    let v = args.next().context("--state needs a path")?;
                    bridge_state = Some(PathBuf::from(v));
                }
                "--gql-endpoint" => {
                    let v = args.next().context("--gql-endpoint needs a URL")?;
                    gql_endpoint = Some(v);
                }
                "--layer-idx" => {
                    let v = args.next().context("--layer-idx needs a u32")?;
                    layer_idx = v.parse::<u32>().context("--layer-idx must be a u32")?;
                }
                "--out" => {
                    let v = args.next().context("--out needs a path")?;
                    out = Some(PathBuf::from(v));
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        let partial_witness =
            partial_witness.ok_or_else(|| anyhow::anyhow!("--partial-witness is required"))?;
        let out = out.ok_or_else(|| anyhow::anyhow!("--out is required"))?;

        Ok(Self {
            partial_witness,
            bridge_state: bridge_state.unwrap_or_else(|| PathBuf::from(DEFAULT_STATE)),
            gql_endpoint: gql_endpoint.unwrap_or_else(|| DEFAULT_GQL_ENDPOINT.to_string()),
            layer_idx,
            out,
        })
    }
}

fn print_help() {
    eprintln!(
        "Usage: bridge-event-witness-builder \
         --partial-witness <path> --out <path> \
         [--state <path>] [--gql-endpoint <url>] [--layer-idx <u32>]"
    );
    eprintln!();
    eprintln!("  --partial-witness <path>  PrivateWitness JSON from bridge-event-private-witness-export.");
    eprintln!("  --out <path>              Output path for the enriched PrivateWitness JSON.");
    eprintln!("  --state <path>            BridgeState JSON path. Default: {}", DEFAULT_STATE);
    eprintln!("  --gql-endpoint <url>      Default: {}", DEFAULT_GQL_ENDPOINT);
    eprintln!("  --layer-idx <u32>         Anchor layer (0 = L1). Only 0 supported in this cut.");
    eprintln!();
    eprintln!("Prints a single-line JSON summary on the last non-empty line of stdout.");
}

#[derive(Serialize)]
struct OutputSummary<'a> {
    schema_version: u32,
    event_message_hash_hex: &'a str,
    block_seq_no: u64,
    key_block_seq_no: u64,
    layer_idx: u32,
    layer_hash_hex: &'a str,
    events_tree_depth: usize,
    block_tree_depth: usize,
    num_active_chain_steps: u32,
    out: String,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // keep stdout clean for the JSON summary
        .init();

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("failed to build tokio runtime: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("bridge-event-witness-builder failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args = CliArgs::parse()?;
    info!("=== bridge-event-witness-builder ===");
    info!("partial_witness: {}", args.partial_witness.display());
    info!("bridge_state:    {}", args.bridge_state.display());
    info!("gql_endpoint:    {}", args.gql_endpoint);
    info!("layer_idx:       {} (only 0 supported for now)", args.layer_idx);
    info!("out:             {}", args.out.display());

    if args.layer_idx != 0 {
        // TODO(L1→L5 escalation): allow layer_idx > 0 and build a
        // multi-step dense_chain whose intermediate Merkle proofs follow
        // `real_chain_builder::build_layer_n_tree`. This first cut keeps
        // the implementation surface small and predictable.
        bail!(
            "--layer-idx {} not yet supported (only L1 / layer_idx=0). \
             See TODO(L1→L5 escalation) in src/main.rs.",
            args.layer_idx
        );
    }

    // ---- Load partial witness -------------------------------------------
    let raw = std::fs::read_to_string(&args.partial_witness)
        .with_context(|| format!("failed to read {}", args.partial_witness.display()))?;
    let mut witness: PrivateWitness = serde_json::from_str(&raw)
        .with_context(|| {
            format!(
                "failed to parse partial PrivateWitness JSON from {}",
                args.partial_witness.display()
            )
        })?;
    if witness.schema_version != SCHEMA_VERSION {
        bail!(
            "partial witness schema_version={} but expected {SCHEMA_VERSION}",
            witness.schema_version,
        );
    }
    let event_seq = witness.block_seq_no;
    let event_repr_hash = parse_hex32("event_message_hash_hex", &witness.event_message_hash_hex)?;
    let dapp = parse_hex32("block_context.account_dapp_id_hex", &witness.block_context.account_dapp_id_hex)?;
    let acc = parse_hex32("block_context.account_id_hex", &witness.block_context.account_id_hex)?;
    info!(
        "partial witness: event_repr_hash={}, block_seq_no={}",
        hex::encode(event_repr_hash),
        event_seq,
    );

    // ---- Load bridge state ----------------------------------------------
    let bridge_state_path_str = args.bridge_state.to_string_lossy().into_owned();
    let bridge_state = BridgeState::load(&bridge_state_path_str, HISTORY_WINDOW_SIZE as usize)
        .with_context(|| format!("failed to load {}", args.bridge_state.display()))?;
    if !bridge_state.initialized {
        bail!(
            "bridge state at {} is uninitialized — no key blocks processed yet",
            args.bridge_state.display()
        );
    }
    info!(
        "bridge state: window_size={}, active_layers={}, last_seen_seq_no={}, last_seen_height={}",
        bridge_state.window_size,
        bridge_state.num_active_layers(),
        bridge_state.stored_last_seen_block_seq_no,
        bridge_state.stored_last_seen_block_height,
    );

    // ---- Connect to GraphQL --------------------------------------------
    let gql = gql_client::create_client(&args.gql_endpoint)
        .with_context(|| format!("failed to construct GQL client for {}", args.gql_endpoint))?;

    // ---- Build events_tree_proof ---------------------------------------
    let events_tree_proof = build_events_tree_proof(&gql, event_seq, &dapp, &acc, &event_repr_hash)
        .await
        .context("building events_tree_proof failed")?;
    info!(
        "events_tree_proof: position={}, depth={}",
        events_tree_proof.position,
        events_tree_proof.siblings_hex.len(),
    );

    // ---- Identify key block for this event -----------------------------
    // Production L1 tree at key block H covers blocks [H - W, ..., H - 1].
    // The unique multiple of W in [event_seq+1, event_seq+W] is the key
    // block whose history_proof[1] is the L1 hash we anchor against.
    //
    // With prover thinning (P > 1), the verifier only persists L1 roots
    // for `(W*P)`-aligned key blocks. Events that fall outside the last
    // W-window of a thinned bundle cannot anchor against a stored root in
    // this first cut — they would require a multi-step chain inside
    // Circuit 4 (see BRIDGE_PROVER_THINNING_SPEC.md §6).
    let w = HISTORY_WINDOW_SIZE;
    let p = THINNING_FACTOR_P;
    let key_block_seq = ((event_seq / w) * w) + w;
    let thinned_key_block_seq = ((event_seq / (w * p)) * (w * p)) + (w * p);
    if key_block_seq != thinned_key_block_seq {
        bail!(
            "event at seq_no {event_seq} falls in W-window ending at key block \
             {key_block_seq}, but with thinning_factor P={p} the verifier only \
             stores L1 roots at (W*P)={}-aligned key blocks (next one: {}). \
             Re-time the event so it lands in the last W={} blocks of a thinned \
             bundle (i.e. event_seq ∈ [{}..{})). \
             Multi-step in-circuit chaining is tracked in \
             BRIDGE_PROVER_THINNING_SPEC.md §6.",
            w * p,
            thinned_key_block_seq,
            w,
            thinned_key_block_seq - w,
            thinned_key_block_seq,
        );
    }
    let window_start = key_block_seq - w;
    let block_offset_in_window = event_seq - window_start;
    info!(
        "key_block_seq={} (thinned, W*P={}-aligned), window=[{}..{}), block_offset_in_window={}",
        key_block_seq,
        w * p,
        window_start,
        key_block_seq,
        block_offset_in_window,
    );

    // ---- Build block_tree_proof -----------------------------------------
    // Replicate the production L1 tree shape exactly:
    //   leaves = [higher_root, prev_same_root, block_leaf_0, ..., block_leaf_{W-1}]
    //          padded to next power of 2.
    let (block_tree_proof, l1_root_self_computed) =
        build_block_tree_proof(&gql, key_block_seq, w, block_offset_in_window)
            .await
            .context("building block_tree_proof failed")?;
    info!(
        "block_tree_proof: position={}, depth={}, root_self_computed={}",
        block_tree_proof.position,
        block_tree_proof.siblings_hex.len(),
        hex::encode(l1_root_self_computed),
    );

    // ---- Build anchor (L1 only) ----------------------------------------
    // Resolve key block's observed_height (the value the verifier stored
    // in heights[] when it applied the bundle), then locate the slot in
    // state.layer_windows[0].
    let key_block_height = fetch_block_observed_height(&gql, key_block_seq)
        .await
        .with_context(|| format!("fetching observed_height for key block {key_block_seq}"))?;
    info!(
        "key block {} observed_height = {}",
        key_block_seq, key_block_height
    );

    let l1_slot = bridge_state.slot_for_event_height(1, key_block_height).ok_or_else(|| {
        // TODO(L1→L5 escalation): when the key block's height has rolled
        // out of layer 1's rolling window, escalate to L2/L3/... rather
        // than failing.
        anyhow::anyhow!(
            "key block height {} not found in L1 window {:?} \
             — block has rolled out of the L1 rolling window. \
             L1→L5 escalation not yet implemented (see TODO in src/main.rs).",
            key_block_height,
            bridge_state.layer_windows[0]
                .iter_chronological()
                .map(|(_, h)| h)
                .collect::<Vec<_>>(),
        )
    })?;
    info!("L1 slot for this key block: {}", l1_slot);

    // The circuit publishes a single `final_root` public input. The verifier
    // checks this value off-circuit against its mirror of `layer_windows`,
    // so we only need to pick the chosen layer hash here (no flattened
    // candidate vector, no choice index).
    let chosen_layer_hash = bridge_state.layer_windows[0]
        .iter_chronological()
        .nth(l1_slot)
        .map(|(h, _)| h)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: L1 slot {} found but not present when iterating chronologically",
                l1_slot,
            )
        })?;

    if chosen_layer_hash != l1_root_self_computed {
        // Not necessarily fatal: the self-computed L1 root depends on
        // higher_layer_root / prev_same_layer_root values, both of which
        // we approximate (see TODO in build_block_tree_proof). A mismatch
        // here means the proof, while structurally valid, will not satisfy
        // the circuit. Surface it loudly.
        warn!(
            "L1 root mismatch — verifier mirror = {}, locally rebuilt = {}. \
             The proof will not satisfy the circuit until block_tree_proof \
             reconstruction matches the node's L1 tree shape byte-for-byte.",
            hex::encode(chosen_layer_hash),
            hex::encode(l1_root_self_computed),
        );
    }

    // L1 anchoring: dense_chain has 0 active steps (the L1 root *is* the
    // chosen layer hash, no hops needed). Pad all MAX_CHAIN_LEN slots with
    // inactive links anchored at root_1. Depth must match the L1 tree's
    // proof depth so `verify_chain_of_dense_proofs` accepts the padding.
    let inactive_depth = block_tree_proof.siblings_hex.len();
    let dense_chain_native: Vec<DenseChainLink> = (0..MAX_CHAIN_LEN)
        .map(|_| DenseChainLink::inactive(chosen_layer_hash, inactive_depth))
        .collect();
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
        layer_idx: args.layer_idx,
        height: key_block_height,
        layer_hash_hex: hex::encode(chosen_layer_hash),
        dense_chain: dense_chain_ser,
        num_active_chain_steps: 0,
    };

    let events_tree_depth = events_tree_proof.siblings_hex.len();
    let block_tree_depth = block_tree_proof.siblings_hex.len();

    witness.events_tree_proof = Some(events_tree_proof);
    witness.block_tree_proof = Some(block_tree_proof);
    witness.anchor = Some(anchor);

    // ---- Write enriched witness ----------------------------------------
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&witness)?;
    std::fs::write(&args.out, json)
        .with_context(|| format!("failed to write {}", args.out.display()))?;
    info!("wrote enriched witness to {}", args.out.display());

    // ---- Stdout summary (last non-empty line, dex-style) ---------------
    let summary = OutputSummary {
        schema_version: witness.schema_version,
        event_message_hash_hex: &witness.event_message_hash_hex,
        block_seq_no: witness.block_seq_no,
        key_block_seq_no: key_block_seq,
        layer_idx: args.layer_idx,
        layer_hash_hex: &witness.anchor.as_ref().unwrap().layer_hash_hex,
        events_tree_depth,
        block_tree_depth,
        num_active_chain_steps: 0,
        out: args.out.to_string_lossy().into_owned(),
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

fn parse_hex32(label: &str, s: &str) -> Result<[u8; 32]> {
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
async fn build_events_tree_proof(
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

    // Same iteration order the node uses to derive ext_out_messages_root:
    // outer = tracked_ext_out_messages BTreeMap, inner = each account's vec.
    let mut leaves: Vec<[u8; 32]> = Vec::new();
    for (account_routing, messages) in block.tracked_ext_out_messages.iter() {
        let (route_dapp, route_acc) = account_routing.unpack_for_hash();
        for msg in messages {
            leaves.push(compute_ext_message_leaf_hash(&route_dapp, &route_acc, msg));
        }
    }
    if leaves.is_empty() {
        bail!(
            "block seq={event_seq} has no tracked_ext_out_messages — \
             cannot have emitted a WithdrawalInitiated event there"
        );
    }

    let target_leaf = compute_ext_message_leaf_hash(dapp, acc, event_repr_hash);
    let position = leaves.iter().position(|l| *l == target_leaf).ok_or_else(|| {
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
/// the L1 root we computed (so the caller can sanity-check it against the
/// verifier's mirrored value).
///
/// Tree shape (matches `bridge-prover-lib::real_chain_builder::build_layer1_tree`):
/// ```text
/// leaves = [
///     higher_layer_root,       // L2 root from the most recent L2 key block
///                              //   (multiples of W*W) ≤ key_block_seq — zero
///                              //   only before the first L2 boundary
///     prev_same_layer_root,    // L1 root from previous L1 key block (or zero)
///     block_leaf_0,
///     ...,
///     block_leaf_{W-1},
/// ]
/// padded with zeros to the next power of 2
/// ```
async fn build_block_tree_proof(
    gql: &GqlClient,
    key_block_seq: u64,
    w: u64,
    block_offset_in_window: u64,
) -> Result<(MerkleProofData, [u8; 32])> {
    // higher_layer_root (L2): the most recent L2 root from the most recent
    // L2 key block ≤ this key block — NOT necessarily this key block's own
    // history_proofs[2] (which is zero unless this key block is L2-aligned).
    // Mirrors `bridge_prover_lib::real_chain_builder::build_layer1_tree`.
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
            }
        }
    };

    // prev_same_layer_root (L1 root from previous key block)
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
            }
        }
    };

    // block leaves for the W blocks in [key_block_seq - W, key_block_seq)
    let window_start = key_block_seq - w;
    let mut block_leaves = Vec::with_capacity(w as usize);
    for seq in window_start..key_block_seq {
        let b = gql
            .query_proof_block_by_seqno(seq)
            .await
            .with_context(|| format!("fetching block seq={seq} in L1 window"))?;
        block_leaves.push(b.block_leaf_hash());
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
            leaf_position, leaves.len(), w, block_offset_in_window,
        );
    }

    let (root, siblings) = build_tree_and_proof(&leaves, leaf_position);
    Ok((MerkleProofData {
        position: leaf_position as u32,
        siblings_hex: siblings.iter().map(hex::encode).collect(),
    }, root))
}

/// Resolve a block's `observed_height` (= `common_section.block_height.height()`).
///
/// The verifier stores this value (not seq_no) in `state.heights[]`, so the
/// witness builder needs it to look up the right slot.
async fn fetch_block_observed_height(gql: &GqlClient, seq: u64) -> Result<u64> {
    let block = gql.query_proof_block_by_seqno(seq).await?;
    Ok(block.height)
}
