//! Seed `state/prover_bk_set.json` with the BK set active at a chosen
//! chain height by folding forward from a genesis snapshot.
//!
//! # Why this exists
//!
//! The bridge prover starts from a JSON config carrying the genesis BK set
//! (see `BRIDGE_BK_SET_CONFIG`). Operators sometimes want the daemon to
//! begin its drain from a later point — either to reproduce a historical
//! bug (e.g., replay a specific rotation burst) or to skip cold-start
//! catch-up when standing up a new instance next to an already-live chain.
//!
//! This binary reconstructs the BK set at `--cap-height` using the sound
//! `bk_set_at_height` primitive (pages `bkSetUpdates` chronologically,
//! folds every rotation with `height <= cap-height` onto the genesis
//! anchor), and writes the result as a `ProverBkSet` JSON snapshot with
//! `last_applied_update_seq_no = cap-height`. On next launch the daemon
//! loads this file and its drain fires on the first rotation event with
//! `height > cap-height`.
//!
//! # Why the genesis snapshot is required
//!
//! AN's `bkSetUpdates` is a delta log: it does NOT emit the founding
//! committee as a synthetic `Added` event. Folding from ∅ would silently
//! return the wrong set on any rotating chain (and an empty set on a
//! fresh one). The genesis snapshot is the anchor without which
//! reconstruction is unsound. Provide it via `--genesis PATH` or the
//! `BRIDGE_BK_SET_CONFIG` env var (same file the daemon consumes).
//!
//! # Usage
//!
//! ```sh
//! BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
//! BRIDGE_BK_SET_CONFIG=state/genesis_bk_set.json \
//!     cargo run --release --example seed_prover_bk_set -- \
//!     --cap-height 2608018 \
//!     --out state/prover_bk_set.json
//! ```
//!
//! `--genesis PATH` overrides `BRIDGE_BK_SET_CONFIG` if both are set.
//! After running, launch the daemon as usual; the drain will fire on the
//! first rotation event with `height > --cap-height`.

use anyhow::{bail, Context};
use bridge_gql_fetcher::bk_set_fetcher::{bk_set_at_height, load_bk_set_from_config};
use bridge_gql_fetcher::gql_client::create_client;
use bridge_prover_lib::prover_bk_set::ProverBkSet;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut cap_height: Option<u64> = None;
    let mut out_path: String = "state/prover_bk_set.json".to_string();
    let mut genesis_path: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cap-height" => {
                cap_height = Some(
                    args.next()
                        .context("--cap-height needs a value")?
                        .parse()
                        .context("--cap-height must be u64")?,
                );
            }
            "--genesis" => {
                genesis_path = Some(args.next().context("--genesis needs a path")?);
            }
            "--out" => {
                out_path = args.next().context("--out needs a path")?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: seed_prover_bk_set --cap-height SEQNO [--genesis PATH] [--out PATH]\n\
                     \n\
                     Reconstructs the BK set active at chain height SEQNO by folding\n\
                     `bkSetUpdates` (height <= SEQNO) onto a genesis snapshot, and writes\n\
                     the result as a ProverBkSet JSON file.\n\
                     \n\
                     Options:\n\
                       --cap-height SEQNO   (required) target chain height\n\
                       --genesis PATH       genesis BK set JSON; overrides BRIDGE_BK_SET_CONFIG\n\
                       --out PATH           output file (default: state/prover_bk_set.json)\n\
                     \n\
                     Env:\n\
                       BRIDGE_GQL_ENDPOINT  (required) AN GraphQL endpoint\n\
                       BRIDGE_BK_SET_CONFIG genesis BK set JSON path (fallback for --genesis)"
                );
                return Ok(());
            }
            other => bail!("unknown arg: {other} (try --help)"),
        }
    }

    let cap_height = cap_height.context("--cap-height is required (try --help)")?;
    let genesis_path = genesis_path
        .or_else(|| std::env::var("BRIDGE_BK_SET_CONFIG").ok())
        .context("genesis BK set path required: pass --genesis PATH or set BRIDGE_BK_SET_CONFIG")?;
    let endpoint = std::env::var("BRIDGE_GQL_ENDPOINT")
        .context("BRIDGE_GQL_ENDPOINT not set")?;

    let genesis = load_bk_set_from_config(&genesis_path)
        .with_context(|| format!("load genesis BK set from {genesis_path}"))?;
    println!(
        "loaded genesis BK set: {} signers from {}",
        genesis.len(),
        genesis_path
    );

    let client = create_client(&endpoint).context("create_client failed")?;

    println!(
        "reconstructing BK set at height {} via bk_set_at_height (paged)...",
        cap_height
    );
    let bk_set = bk_set_at_height(&client, genesis, cap_height)
        .await
        .with_context(|| format!("bk_set_at_height({cap_height}) failed"))?;

    let mut indices: Vec<u16> = bk_set.keys().copied().collect();
    indices.sort();
    println!(
        "reconstructed BK set at height {}: {} signers {:?}",
        cap_height,
        bk_set.len(),
        indices
    );

    let pbs = ProverBkSet::from_pubkeys(&bk_set, cap_height);
    println!(
        "Poseidon commitment: {}\nlast_applied_update_seq_no: {}",
        hex::encode(pbs.commitment),
        pbs.last_applied_update_seq_no
    );

    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(&pbs)?;
    std::fs::write(&out_path, json).with_context(|| format!("write {out_path}"))?;
    println!("wrote {out_path}");

    Ok(())
}
