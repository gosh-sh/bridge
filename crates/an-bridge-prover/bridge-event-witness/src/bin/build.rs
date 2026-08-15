//! `bridge-event-witness-builder` 
//!
//! Reads a *partial* `PrivateWitness` JSON (the one produced by
//! `bridge-event-private-witness-export`), pulls the
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
//!     Built with the production L1 tree shape: `2 + W` leaves
//!     `[higher_layer_root, prev_same_layer_root, block_leaf_0, ...,
//!     block_leaf_{W-1}]` padded to the next power of 2 (at W=128:
//!     130 leaves → 256 padded → 8-deep proofs). Same layout
//!     `bridge-prover-lib::real_chain_builder::build_layer1_tree` uses.
//!   * `anchor` — references the L1 layer hash the verifier has mirrored
//!     for the thinned key block `K = ⌈event_seq/(W·P)⌉·(W·P)`. Carries
//!     the chosen layer hash (the value the circuit publishes as
//!     `PUB_FINAL_ROOT` = `R_1@K`) and the `dense_chain`. When the
//!     event's own W-aligned key block `H_e = ⌈event_seq/W⌉·W` differs
//!     from `K`, the chain walks `hops = (K − H_e)/W` forward-hops at
//!     layer 1 (each hop opens slot 1 = `prev_same_layer_root` of the
//!     next L1 tree). `hops ∈ {0, …, P−1}`, comfortably within
//!     `MAX_CHAIN_LEN`. Remaining slots are inactive padding anchored at
//!     the chosen layer hash.
//!
//! ### L1 and explicit L2 anchoring; L(N≥3) auto-escalation deferred
//!
//! This cut supports two explicit anchor layers, selected via
//! `--anchor-layer <1|2>` (default 1):
//!
//! * `--anchor-layer 1` (L1) — horizontal L1 walk from `H_e` to the thinned
//!   key block `K = ⌈event_seq/(W·P)⌉·(W·P)`. Wait time budget: up to
//!   `W·P − 1` blocks (at W=128, P=4 → ≤ 511 blocks).
//! * `--anchor-layer 2` (L2) — one vertical L1→L2 rung to
//!   `T_2 = ⌈event_seq/W²⌉·W²`. Wait time budget: up to `W² − 1` blocks
//!   (at W=128 → ≤ 16383 blocks). Because this is ≈ 2 hours of verifier
//!   catch-up on shellnet cadence, the CLI refuses to run at L2 unless
//!   the caller also passes `--i-know-the-wait`.
//!
//! In both cases, if the chosen key block has rolled out of the relevant
//! rolling window (i.e. no slot in `state.layer_windows[target_layer-1]`
//! matches the key block's observed_height), the command fails with a clear
//! error rather than silently producing an un-anchored witness.
//!
//! TODO(L1→L5 auto-escalation): implement transparent escalation for old
//! events whose L1 slot has rolled out: walk up through L2/L3/... using
//! additional vertical rungs from `real_chain_builder::build_layer_n_leaves`.
//! Follow-up to this pass — first land explicit L1/L2 support, then add the
//! escalation policy (bootstrap-from-middle scanning).
//!
//! ### Mode and exit codes
//!
//! ```text
//! bridge-event-witness-builder
//!   --partial-witness  <path>                  (required)
//!   --state            ./state/prover_state.json
//!   --gql-endpoint     http://localhost/graphql
//!   --anchor-layer     1                       (1 = L1 [default], 2 = L2)
//!   --layer-idx        0                       (0-indexed alias for --anchor-layer)
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
use bridge_prover_lib::real_chain_builder;
use bridge_gql_fetcher::gql_client::{self, GqlClient};

use bridge_event_witness::schema::{
    AnchorRef, DenseChainLinkSer, MerkleProofData, PrivateWitness, SCHEMA_VERSION,
};

use gosh_dense_balanced_tree::{DenseChainLink, MAX_CHAIN_LEN};

const HISTORY_WINDOW_SIZE: u64 =
    bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64;

const THINNING_FACTOR_P: u64 = bridge_prover_lib::THINNING_FACTOR_P;

/// Default path of the bridge-state JSON file the witness builder reads.
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
    /// 1-indexed target anchor layer: 1 = L1, 2 = L2. Only these two are
    /// supported in this cut; higher layers are future work.
    anchor_layer: u8,
    /// Opt-in bypass for the wait-time guard on anchor layers whose
    /// worst-case verifier catch-up is impractical. Required for
    /// `--anchor-layer 2` (up to `W² − 1` blocks ≈ hours on shellnet).
    /// Ignored for `--anchor-layer 1` (wait ≤ W·P − 1 blocks).
    i_know_the_wait: bool,
    out: PathBuf,
}

impl CliArgs {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut partial_witness: Option<PathBuf> = None;
        let mut bridge_state: Option<PathBuf> = None;
        let mut gql_endpoint: Option<String> = None;
        // The 1-indexed anchor layer is the user-facing flag. `--layer-idx`
        // (0-indexed) is accepted for backwards compat with existing Python
        // drivers; if both flags are given they must resolve to the same
        // value.
        let mut anchor_layer_1indexed: Option<u8> = None;
        let mut layer_idx_0indexed: Option<u32> = None;
        let mut i_know_the_wait = false;
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
                "--anchor-layer" => {
                    let v = args.next().context("--anchor-layer needs 1 or 2")?;
                    let n: u8 = v.parse::<u8>().context("--anchor-layer must be 1 or 2")?;
                    anchor_layer_1indexed = Some(n);
                }
                "--layer-idx" => {
                    let v = args.next().context("--layer-idx needs a u32")?;
                    layer_idx_0indexed =
                        Some(v.parse::<u32>().context("--layer-idx must be a u32")?);
                }
                "--i-know-the-wait" => {
                    i_know_the_wait = true;
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

        // Resolve anchor_layer from either flag. Default 1 (L1).
        let anchor_layer: u8 = match (anchor_layer_1indexed, layer_idx_0indexed) {
            (Some(a), Some(b)) => {
                let from_idx = (b as u8)
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("--layer-idx {} overflows u8", b))?;
                if a != from_idx {
                    bail!(
                        "--anchor-layer {} and --layer-idx {} disagree ({} vs {}). \
                         Pass only one, or make them consistent.",
                        a, b, a, from_idx,
                    );
                }
                a
            }
            (Some(a), None) => a,
            (None, Some(b)) => (b as u8)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("--layer-idx {} overflows u8", b))?,
            (None, None) => 1,
        };
        if anchor_layer < 1 || anchor_layer > 2 {
            bail!(
                "--anchor-layer must be 1 (L1) or 2 (L2). Got {}. \
                 Higher layers are future work — see TODO(L1→L5 auto-escalation) \
                 in bin/build.rs.",
                anchor_layer,
            );
        }

        Ok(Self {
            partial_witness,
            bridge_state: bridge_state.unwrap_or_else(|| PathBuf::from(DEFAULT_STATE)),
            gql_endpoint: gql_endpoint.unwrap_or_else(|| DEFAULT_GQL_ENDPOINT.to_string()),
            anchor_layer,
            i_know_the_wait,
            out,
        })
    }
}

fn print_help() {
    eprintln!(
        "Usage: bridge-event-witness-builder \
         --partial-witness <path> --out <path> \
         [--state <path>] [--gql-endpoint <url>] \
         [--anchor-layer <1|2>] [--layer-idx <u32>] [--i-know-the-wait]"
    );
    eprintln!();
    eprintln!("  --partial-witness <path>  PrivateWitness JSON from bridge-event-private-witness-export.");
    eprintln!("  --out <path>              Output path for the enriched PrivateWitness JSON.");
    eprintln!("  --state <path>            BridgeState JSON path. Default: {}", DEFAULT_STATE);
    eprintln!("  --gql-endpoint <url>      Default: {}", DEFAULT_GQL_ENDPOINT);
    eprintln!("  --anchor-layer <1|2>      1 = L1 anchor (default), 2 = L2 anchor.");
    eprintln!("                            Wait budget: L1 ≤ W·P−1 blocks, L2 ≤ W²−1 blocks.");
    eprintln!("  --layer-idx <u32>         0-indexed alias for --anchor-layer (0 = L1, 1 = L2).");
    eprintln!("                            Kept for backwards compat with existing Python drivers.");
    eprintln!("  --i-know-the-wait         Bypass the wait-time guard on impractical anchor layers.");
    eprintln!("                            Required for --anchor-layer 2 (up to W²−1 blocks of");
    eprintln!("                            verifier catch-up, ≈ hours on shellnet cadence).");
    eprintln!();
    eprintln!("Prints a single-line JSON summary on the last non-empty line of stdout.");
}

#[derive(Serialize)]
struct OutputSummary<'a> {
    schema_version: u32,
    event_message_hash_hex: &'a str,
    block_seq_no: u64,
    /// Event's own W-aligned key block `H_e` (covers the block_tree_proof).
    key_block_seq_no: u64,
    /// Verifier-side anchor key block seqno: `K = ⌈event_seq/(W·P)⌉·(W·P)` for
    /// L1 anchoring, `T_2 = ⌈event_seq/W²⌉·W²` for L2 anchoring. Field name
    /// kept as `thinned_key_block_seq_no` for backwards compatibility with
    /// existing consumers of this JSON summary.
    thinned_key_block_seq_no: u64,
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
    info!(
        "anchor_layer:    L{} (layer_idx={}); supported: L1, L2",
        args.anchor_layer,
        args.anchor_layer - 1,
    );
    info!("out:             {}", args.out.display());

    // ---- Wait-time guard (Phase 3) --------------------------------------
    //
    // Anchor layers > 1 have a worst-case verifier catch-up budget that's
    // impractical to sit through in an interactive/E2E-test context.
    // At W=128, P=4 and shellnet's observed ~0.5s/seq_no verifier cadence:
    //   L1: worst case  W·P − 1 =   511 blocks ≈  4 min → no guard.
    //   L2: worst case  W²  − 1 = 16383 blocks ≈ 2 hours → guarded.
    //
    // The guard is advisory-only (all the actual waiting happens in the
    // caller / Python orchestrator waiting for verifier_state.json). It
    // exists so callers picking `--anchor-layer 2` know what they're
    // signing up for and confirm intent with `--i-know-the-wait`.
    guard_wait_time(args.anchor_layer, args.i_know_the_wait, HISTORY_WINDOW_SIZE)?;

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

    // ---- Identify H_e (event's L1 key block) ---------------------------
    // Production L1 tree at key block H covers blocks [H - W, ..., H - 1].
    // The unique multiple of W in [event_seq+1, event_seq+W] is the key
    // block whose history_proof[1] is the L1 hash containing this event's
    // block leaf — we call this `H_e` (`key_block_seq`).
    //
    // For L1 anchoring the verifier's stored KB is `K = ⌈event_seq/(W·P)⌉·(W·P)`
    // and we build a horizontal L1 walk H_e → K. For L2 anchoring the
    // verifier's stored KB is `T_2 = ⌈event_seq/W²⌉·W²` and we build a single
    // vertical L1→L2 rung. Both paths are dispatched through
    // `real_chain_builder::build_event_anchor_chain`.
    let w = HISTORY_WINDOW_SIZE;
    let p = THINNING_FACTOR_P;
    let key_block_seq = ((event_seq / w) * w) + w;
    let window_start = key_block_seq - w;
    let block_offset_in_window = event_seq - window_start;
    info!(
        "H_e={} (event's W-aligned KB, window=[{}..{}), offset={})",
        key_block_seq, window_start, key_block_seq, block_offset_in_window,
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

    // ---- Build the event-anchor chain (L1 horizontal or L1→L2 vertical) -
    // The composed helper picks the topology from `args.anchor_layer` and
    // returns the active links + the reconstructed final root.
    let chain_result = real_chain_builder::build_event_anchor_chain(
        &gql,
        event_seq,
        l1_root_self_computed,
        args.anchor_layer,
        w,
        p,
    )
    .await
    .with_context(|| {
        format!(
            "building event-anchor chain (target_layer=L{}) failed",
            args.anchor_layer
        )
    })?;
    let num_active = chain_result.active_links.len();
    if num_active > MAX_CHAIN_LEN {
        bail!(
            "internal: {} active chain links exceed MAX_CHAIN_LEN={} \
             (anchor_layer=L{}, event_seq={}, W={}, P={}, anchor_kb={})",
            num_active, MAX_CHAIN_LEN, args.anchor_layer, event_seq, w, p,
            chain_result.anchor_kb_seqno,
        );
    }
    info!(
        "chain built: anchor_layer=L{}, anchor_kb={}, active_links={}, final_root={}",
        args.anchor_layer,
        chain_result.anchor_kb_seqno,
        num_active,
        hex::encode(chain_result.final_chain_root),
    );

    // ---- Look up the verifier's mirrored layer hash --------------------
    // The verifier stores L(target_layer) roots keyed by the anchor KB's
    // observed_height. Layer numbering here is 1-indexed (L1, L2, ...).
    let anchor_key_block_seq = chain_result.anchor_kb_seqno;
    let key_block_height = fetch_block_observed_height(&gql, anchor_key_block_seq)
        .await
        .with_context(|| {
            format!(
                "fetching observed_height for anchor key block {anchor_key_block_seq} \
                 (target_layer=L{})",
                args.anchor_layer,
            )
        })?;
    info!(
        "anchor key block {} observed_height = {}",
        anchor_key_block_seq, key_block_height,
    );

    let target_layer_idx0 = (args.anchor_layer - 1) as usize;
    let slot = bridge_state
        .slot_for_event_height(args.anchor_layer, key_block_height)
        .ok_or_else(|| {
            // TODO(L1→L5 auto-escalation): if the anchor KB's height has
            // rolled out of L(anchor_layer)'s rolling window, transparently
            // walk up to the next layer rather than failing here.
            anyhow::anyhow!(
                "anchor key block height {} not found in L{} window {:?} \
                 — block has rolled out of the L{} rolling window. \
                 Auto-escalation to higher layers is not yet implemented; \
                 rerun with a higher `--anchor-layer` if the state has \
                 progressed to that layer.",
                key_block_height,
                args.anchor_layer,
                bridge_state.layer_windows[target_layer_idx0]
                    .iter_chronological()
                    .map(|(_, h)| h)
                    .collect::<Vec<_>>(),
                args.anchor_layer,
            )
        })?;
    info!(
        "L{} slot for this anchor key block: {}",
        args.anchor_layer, slot,
    );

    let chosen_layer_hash = bridge_state.layer_windows[target_layer_idx0]
        .iter_chronological()
        .nth(slot)
        .map(|(h, _)| h)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: L{} slot {} found but not present when iterating chronologically",
                args.anchor_layer, slot,
            )
        })?;

    if chosen_layer_hash != chain_result.final_chain_root {
        // Not necessarily fatal: the self-computed roots depend on
        // higher_layer_root / prev_same_layer_root values fetched from
        // GQL. A mismatch here means the proof, while structurally
        // valid, will not satisfy the circuit. Surface it loudly.
        warn!(
            "final chain root mismatch — verifier mirror (L{} root @ anchor KB) = {}, \
             locally rebuilt = {}. The proof will not satisfy the circuit until \
             every tree along the chain matches the node's construction byte-for-byte.",
            args.anchor_layer,
            hex::encode(chosen_layer_hash),
            hex::encode(chain_result.final_chain_root),
        );
    }

    // Assemble MAX_CHAIN_LEN links: `num_active` chain rungs followed by
    // inactive padding anchored at the chosen (verifier-mirrored) root.
    // Depth must match each active link's Merkle proof depth so
    // `verify_chain_of_dense_proofs` accepts the padding.
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
        layer_idx: (args.anchor_layer - 1) as u32,
        height: key_block_height,
        layer_hash_hex: hex::encode(chosen_layer_hash),
        dense_chain: dense_chain_ser,
        num_active_chain_steps: num_active as u32,
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
        thinned_key_block_seq_no: anchor_key_block_seq,
        layer_idx: (args.anchor_layer - 1) as u32,
        layer_hash_hex: &witness.anchor.as_ref().unwrap().layer_hash_hex,
        events_tree_depth,
        block_tree_depth,
        num_active_chain_steps: num_active as u32,
        out: args.out.to_string_lossy().into_owned(),
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// Empirical verifier catch-up rate on shellnet, used only for the
/// human-readable wait estimate in [`guard_wait_time`]. Actual observed
/// throughput on 2026-08-15: verifier moved 1536 seq_nos in 9 min ≈
/// 0.35 s/seq_no; we round up to 0.5 to stay on the conservative side.
const VERIFIER_SECS_PER_SEQNO: f64 = 0.5;

/// Wait-time guard threshold: if worst-case verifier catch-up at the
/// chosen anchor layer exceeds this, require `--i-know-the-wait`.
/// 15 min is deliberately generous — L1 (≤ ~4 min) never trips it, L2
/// (≥ ~2 h) always does.
const WAIT_TIME_WARN_THRESHOLD_SECS: f64 = 15.0 * 60.0;

/// Return the worst-case verifier catch-up window (in seq_no units) for
/// the chosen anchor layer. See the L1/L2 wait-budget lines in the
/// module docblock.
fn worst_case_wait_blocks(anchor_layer: u8, w: u64, p: u64) -> u64 {
    match anchor_layer {
        1 => w * p - 1,
        2 => w * w - 1,
        // Higher layers are rejected earlier by CliArgs::parse, but
        // return a conservative bound here so this function stays total.
        n => w.saturating_pow(n as u32).saturating_sub(1),
    }
}

/// Warn about the wait time for the chosen anchor layer; error out if
/// the worst case exceeds [`WAIT_TIME_WARN_THRESHOLD_SECS`] and the
/// caller has not passed `--i-know-the-wait`.
fn guard_wait_time(anchor_layer: u8, i_know_the_wait: bool, w: u64) -> Result<()> {
    let p = THINNING_FACTOR_P;
    let max_blocks = worst_case_wait_blocks(anchor_layer, w, p);
    let max_secs = (max_blocks as f64) * VERIFIER_SECS_PER_SEQNO;

    if max_secs <= WAIT_TIME_WARN_THRESHOLD_SECS {
        // L1 lands here — no warning worth the noise.
        return Ok(());
    }

    let max_minutes = max_secs / 60.0;
    if !i_know_the_wait {
        bail!(
            "--anchor-layer {} has an impractical wait budget: worst case {} blocks \
             (≈ {:.0} min at ~{:.1}s per seq_no verifier catch-up). Re-run with \
             --i-know-the-wait if this is intentional; typical E2E callers should \
             stick with --anchor-layer 1 (L1, ≤ W·P−1 blocks ≈ 4 min).",
            anchor_layer, max_blocks, max_minutes, VERIFIER_SECS_PER_SEQNO,
        );
    }
    // Opt-in acknowledged; log the estimate but proceed.
    warn!(
        "anchor_layer=L{}: worst-case verifier catch-up ≈ {} blocks (~{:.0} min); \
         proceeding because --i-know-the-wait was passed",
        anchor_layer, max_blocks, max_minutes,
    );
    Ok(())
}

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
