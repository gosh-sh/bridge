//! Seed `state/prover_bk_set.json` to a historical pre-burst point so the
//! daemon's drain loop replays a known cluster of bkSetUpdates.
//!
//! Walks shellnet's `bkSetUpdates` history, applies every change with
//! `height <= --cap-height`, normalizes the resulting pubkey table to
//! 48-byte compressed form, computes the Poseidon commitment, and writes
//! `state/prover_bk_set.json` with `last_applied_update_seq_no = cap`.
//!
//! Usage (defaults reproduce the burst-4 replay):
//!
//! ```sh
//! BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
//!     cargo run --release --example seed_pre_burst -- \
//!     --cap-height 2608018 \
//!     --out state/prover_bk_set.json
//! ```
//!
//! After running, launch the daemons as usual; the drain will fire on the
//! first rotation event with `height > 2608018` and walk forward through the
//! cluster.

use std::collections::HashMap;

use anyhow::{bail, Context};
use bridge_prover_lib::bk_set_fetcher::{
    normalize_bk_set_pubkeys, parse_bk_set_changes_pub, BK_CHANGE_VARIANT_ADDED,
    BK_CHANGE_VARIANT_REMOVED,
};
use bridge_prover_lib::gql_client::create_client;
use bridge_prover_lib::prover_bk_set::ProverBkSet;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut cap_height: u64 = 2_608_018; // last event of burst 3 on shellnet
    let mut out_path: String = "state/prover_bk_set.json".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cap-height" => {
                cap_height = args
                    .next()
                    .context("--cap-height needs a value")?
                    .parse()
                    .context("--cap-height must be u64")?;
            }
            "--out" => {
                out_path = args.next().context("--out needs a path")?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: seed_pre_burst [--cap-height SEQNO] [--out PATH]\n\
                     defaults: --cap-height 2608018, --out state/prover_bk_set.json"
                );
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    let endpoint = std::env::var("BRIDGE_GQL_ENDPOINT")
        .context("BRIDGE_GQL_ENDPOINT not set")?;
    let client = create_client(&endpoint).context("create_client failed")?;

    // Collect the full history (first 500 + last 500, dedup'd by block_id),
    // mirroring `bk_set_fetcher::fetch_bk_set`.
    let mut updates = client.query_bk_set_updates_light(500, true).await?;
    let recent = client.query_bk_set_updates_light(500, false).await?;
    let existing_ids: std::collections::HashSet<String> =
        updates.iter().map(|u| u.block_id.clone()).collect();
    for u in recent {
        if !existing_ids.contains(&u.block_id) {
            updates.push(u);
        }
    }
    if updates.is_empty() {
        bail!("no bkSetUpdates returned");
    }

    // Filter to height <= cap, then apply in chronological order so a remove
    // following an add at the same index resolves correctly.
    let mut capped: Vec<_> = updates
        .into_iter()
        .filter(|u| u.height.map(|h| h <= cap_height).unwrap_or(false))
        .collect();
    capped.sort_by_key(|u| u.height.unwrap_or(0));
    if capped.is_empty() {
        bail!("no bkSetUpdates with height <= {cap_height}");
    }
    let max_height = capped
        .last()
        .and_then(|u| u.height)
        .unwrap_or(cap_height);
    println!(
        "applying {} updates (heights {:?}..={:?})",
        capped.len(),
        capped.first().and_then(|u| u.height),
        capped.last().and_then(|u| u.height)
    );

    let mut bk_set: HashMap<u16, Vec<u8>> = HashMap::new();
    for update in &capped {
        if update.bk_set_update_hex.is_empty() {
            continue;
        }
        let blob = hex::decode(&update.bk_set_update_hex)
            .context("decode bk_set_update_hex")?;
        for (variant, idx, pk) in parse_bk_set_changes_pub(&blob) {
            match variant {
                BK_CHANGE_VARIANT_ADDED => {
                    bk_set.insert(idx, pk);
                }
                BK_CHANGE_VARIANT_REMOVED => {
                    bk_set.remove(&idx);
                }
                _ => {}
            }
        }
    }

    let bk_set = normalize_bk_set_pubkeys(bk_set)?;
    let mut indices: Vec<u16> = bk_set.keys().copied().collect();
    indices.sort();
    println!(
        "reconstructed BK set: {} signers {:?}",
        bk_set.len(),
        indices
    );

    let pbs = ProverBkSet::from_pubkeys(&bk_set, max_height);
    println!(
        "Poseidon commitment: {}\nlast_applied_update_seq_no: {}",
        hex::encode(pbs.commitment),
        pbs.last_applied_update_seq_no
    );

    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(&pbs)?;
    std::fs::write(&out_path, json).context("write prover_bk_set.json")?;
    println!("wrote {out_path}");

    Ok(())
}
