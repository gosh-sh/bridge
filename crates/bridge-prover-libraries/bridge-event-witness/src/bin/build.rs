//! `bridge-event-witness-builder`
//!
//! Thin CLI wrapper over [`bridge_event_witness::enrich::enrich_witness`].
//! Handles argument parsing, partial-witness JSON loading, bridge-state
//! JSON loading, GraphQL client construction, and output serialization.
//! All enrichment logic (anchor-layer resolution, wait-time guard,
//! events/block/anchor-chain tree building) lives in the library module.
//!
//! ### Fields the enrichment library fills in
//!
//! * `events_tree_proof` — Poseidon Merkle proof from
//!   `ext_msg_leaf = Poseidon96(dapp || account || repr_hash)` up to the
//!   block's `ext_out_messages_root`. Built from the same
//!   `tracked_ext_out_messages` map the node uses (see
//!   `history_proof::dense_merkle_proof` in the acki-nacki node).
//!
//! * `block_tree_proof` — Poseidon Merkle proof from
//!   `block_leaf = Poseidon96(block_id || envelope_hash || ext_out_root)`
//!   up to `root_1` (the L1 window root the verifier mirrors). L1 tree
//!   shape: `2 + W` leaves
//!   `[higher_layer_root, prev_same_layer_root, block_leaf_0, ..., block_leaf_{W-1}]`
//!   padded to the next power of 2 (W=128 → 130 leaves → 256 padded →
//!   8-deep proofs). Matches `real_chain_builder::build_layer1_tree`.
//!
//! * `anchor` — references the L(target_layer) hash the verifier mirrors.
//!   Carries the chosen layer hash (published by the circuit as
//!   `PUB_FINAL_ROOT = R_L@K_L`, `L = target_layer`) plus `dense_chain`.
//!   Shape depends on `L`:
//!   * `L = 1`: horizontal walk from
//!     `H_e = ⌊event_seq/W⌋·W + W` (KB emitting the event batch's L1
//!     root; see `l1_anchor_boundaries`) to the thinned L1 KB
//!     `K = ⌊event_seq/(W·P)⌋·(W·P) + (W·P)` the verifier mirrors, with
//!     `hops = (K − H_e)/W ∈ {0, …, P−1}` rungs each opening slot 1
//!     (`prev_same_layer_root`) of the next L1 tree.
//!   * `L = n ≥ 2`: vertical stack of `n − 1` new-layer rungs L1→…→L(n)
//!     landing at `T_n = ⌊event_seq/W^n⌋·W^n + W^n` (see
//!     `l_n_anchor_boundaries`). Each rung opens the previous-layer root
//!     at data-leaf position `2 + (W − 1 − k_m)` of the next-layer tree
//!     (chronological order per `HistoryBlockData::calculate_root_hash`).
//!   Unused chain slots are inactive padding at the chosen layer hash.
//!
//! ### `--anchor-layer` semantics
//!
//! * `1` (default, **strict**) — target L1. Wait budget ≤ `W·P − 1` blocks
//!   (W=128, P=8 → ≤ 1023).
//! * `n ∈ {2, …, MAX_LAYERS}` (**strict**) — target L(n). Wait budget
//!   ≤ `W^n − 1` blocks (L2 ≈ 2 h at shellnet cadence; L3 ≈ 12 d).
//!   Requires `--i-know-the-wait` to guard against surprise multi-hour
//!   waits.
//! * `auto` — target L1 whenever the verifier still holds an L1 slot for
//!   the event's `K`. If L1 rolled out (event too old for the L1 rolling
//!   window), escalate through `L2, L3, …, L(num_active_layers)` and take
//!   the first layer whose window still covers `T_n`. Implies
//!   `--i-know-the-wait`. Fails if every active layer rolled out, or if
//!   L1 rolled out and `num_active_layers < 2`.
//!
//! Strict mode fails cleanly if the chosen KB has rolled out of
//! `state.layer_windows[L−1]` (no silent un-anchored witness).
//!
//! ### CLI
//!
//! ```text
//! bridge-event-witness-builder
//!   --partial-witness <path>                    (required)
//!   --state           ./state/prover_state.json
//!   --gql-endpoint    http://localhost/graphql
//!   --anchor-layer    1|n|auto                  (or --layer-idx: 0-indexed alias)
//!   --out             <path>                    (required)
//! ```
//!
//! Exit 0 on success. One-line JSON summary on the last non-empty stdout
//! line (dex-tooling style); logging on stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tracing::{error, info};

use bridge_gql_fetcher::gql_client;
use bridge_prover_lib::bridge_state::{BridgeState, MAX_LAYERS};

use bridge_event_witness::enrich::{enrich_witness, AnchorLayerMode, HISTORY_WINDOW_SIZE};
use bridge_event_witness::schema::PrivateWitness;

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
    /// User's requested anchor-layer mode. See [`AnchorLayerMode`].
    anchor_mode: AnchorLayerMode,
    /// Opt-in bypass for the wait-time guard on anchor layers whose
    /// worst-case verifier catch-up is impractical. Required for
    /// `--anchor-layer 2` (up to `W² − 1` blocks ≈ hours on shellnet).
    /// Ignored for `--anchor-layer 1` and `--anchor-layer auto`
    /// (auto-escalation to L2 counts as implicit acknowledgement).
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
        // value. `--anchor-layer auto` selects [`AnchorLayerMode::Auto`]
        // and cannot be combined with `--layer-idx`.
        let mut anchor_mode_arg: Option<AnchorLayerMode> = None;
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
                    let v = args.next().context("--anchor-layer needs 1, 2, or auto")?;
                    let mode = if v.eq_ignore_ascii_case("auto") {
                        AnchorLayerMode::Auto
                    } else {
                        let n: u8 = v.parse::<u8>().with_context(|| {
                            format!("--anchor-layer must be 1, 2, or 'auto' (got {v:?})")
                        })?;
                        AnchorLayerMode::Explicit(n)
                    };
                    anchor_mode_arg = Some(mode);
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

        // Resolve anchor_mode from the two flags. Default is explicit L1
        // (kept for backwards compat with the existing Python drivers
        // that pass `--layer-idx 0`). Auto-escalation is strictly opt-in
        // via `--anchor-layer auto`.
        let anchor_mode: AnchorLayerMode = match (anchor_mode_arg, layer_idx_0indexed) {
            (Some(AnchorLayerMode::Auto), Some(_)) => bail!(
                "--anchor-layer auto cannot be combined with --layer-idx. \
                 Auto mode picks the layer at runtime; --layer-idx forces a specific one."
            ),
            (Some(AnchorLayerMode::Explicit(a)), Some(b)) => {
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
                AnchorLayerMode::Explicit(a)
            }
            (Some(mode), None) => mode,
            (None, Some(b)) => {
                let n = (b as u8)
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("--layer-idx {} overflows u8", b))?;
                AnchorLayerMode::Explicit(n)
            }
            (None, None) => AnchorLayerMode::Explicit(1),
        };
        if let AnchorLayerMode::Explicit(n) = anchor_mode {
            if n < 1 || n as usize > MAX_LAYERS {
                bail!(
                    "--anchor-layer must be in 1..={} (MAX_LAYERS) or 'auto'. Got {}.",
                    MAX_LAYERS, n,
                );
            }
        }

        Ok(Self {
            partial_witness,
            bridge_state: bridge_state.unwrap_or_else(|| PathBuf::from(DEFAULT_STATE)),
            gql_endpoint: gql_endpoint.unwrap_or_else(|| DEFAULT_GQL_ENDPOINT.to_string()),
            anchor_mode,
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
         [--anchor-layer <1..=MAX_LAYERS|auto>] [--layer-idx <u32>] [--i-know-the-wait]"
    );
    eprintln!();
    eprintln!("  --partial-witness <path>  PrivateWitness JSON from bridge-event-private-witness-export.");
    eprintln!("  --out <path>              Output path for the enriched PrivateWitness JSON.");
    eprintln!("  --state <path>            BridgeState JSON path. Default: {}", DEFAULT_STATE);
    eprintln!("  --gql-endpoint <url>      Default: {}", DEFAULT_GQL_ENDPOINT);
    eprintln!("  --anchor-layer <arg>      1 = L1 anchor (default, strict),");
    eprintln!("                            n = L(n) anchor for 2 ≤ n ≤ {} (strict),", MAX_LAYERS);
    eprintln!("                            auto = try L1, escalate through L2..L(num_active_layers)");
    eprintln!("                                   until a layer's window still covers the event.");
    eprintln!("                            Wait budget: L(n) ≤ W^n − 1 blocks");
    eprintln!("                            (L1 ≈ 4 min, L2 ≈ 2 h, L3 ≈ ~12 d on shellnet).");
    eprintln!("  --layer-idx <u32>         0-indexed alias for explicit --anchor-layer");
    eprintln!("                            (0 = L1, 1 = L2, …). Not accepted with 'auto'.");
    eprintln!("                            Kept for backwards compat with existing Python drivers.");
    eprintln!("  --i-know-the-wait         Bypass the wait-time guard on impractical anchor layers.");
    eprintln!("                            Required for explicit --anchor-layer ≥ 2 (up to W^n − 1");
    eprintln!("                            blocks of verifier catch-up).");
    eprintln!("                            Not required with 'auto' — escalation is opt-in.");
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
        "anchor_mode:     {:?}; supported: L1..=L{}",
        args.anchor_mode, MAX_LAYERS,
    );
    info!("out:             {}", args.out.display());

    // ---- Load partial witness -------------------------------------------
    let raw = std::fs::read_to_string(&args.partial_witness)
        .with_context(|| format!("failed to read {}", args.partial_witness.display()))?;
    let partial: PrivateWitness = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse partial PrivateWitness JSON from {}",
            args.partial_witness.display()
        )
    })?;

    // ---- Load bridge state ----------------------------------------------
    let bridge_state_path_str = args.bridge_state.to_string_lossy().into_owned();
    let bridge_state = BridgeState::load(&bridge_state_path_str, HISTORY_WINDOW_SIZE as usize)
        .with_context(|| format!("failed to load {}", args.bridge_state.display()))?;

    // ---- Connect to GraphQL --------------------------------------------
    let gql = gql_client::create_client(&args.gql_endpoint)
        .with_context(|| format!("failed to construct GQL client for {}", args.gql_endpoint))?;

    // ---- Enrich (library call) ------------------------------------------
    let enriched = enrich_witness(
        &gql,
        &bridge_state,
        partial,
        args.anchor_mode,
        args.i_know_the_wait,
    )
    .await
    .context("witness enrichment failed")?;

    // ---- Write enriched witness ----------------------------------------
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&enriched.witness)?;
    std::fs::write(&args.out, json)
        .with_context(|| format!("failed to write {}", args.out.display()))?;
    info!("wrote enriched witness to {}", args.out.display());

    // ---- Stdout summary (last non-empty line, dex-style) ---------------
    let s = &enriched.summary;
    let summary = OutputSummary {
        schema_version: s.schema_version,
        event_message_hash_hex: &s.event_message_hash_hex,
        block_seq_no: s.block_seq_no,
        key_block_seq_no: s.key_block_seq_no,
        thinned_key_block_seq_no: s.thinned_key_block_seq_no,
        layer_idx: s.layer_idx,
        layer_hash_hex: &s.layer_hash_hex,
        events_tree_depth: s.events_tree_depth,
        block_tree_depth: s.block_tree_depth,
        num_active_chain_steps: s.num_active_chain_steps,
        out: args.out.to_string_lossy().into_owned(),
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}
