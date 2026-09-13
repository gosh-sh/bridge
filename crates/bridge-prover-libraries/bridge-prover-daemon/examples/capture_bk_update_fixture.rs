//! Capture a live-chain BK-set-update fixture for
//! [`bridge_prover_lib::tests::parity_bk_update`].
//!
//! Walks `bkSetUpdates` on the endpoint given by `BRIDGE_GQL_ENDPOINT`,
//! folds every rotation with `height < --height` onto `--genesis-bk-set` to
//! reconstruct `old_pubkeys`, applies the target rotation's delta to derive
//! `expected_new_pubkeys`, fetches the target block's 16-leaf tree, and
//! writes the JSON fixture the parity test consumes.
//!
//! Usage:
//!
//! ```sh
//! BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
//!     cargo run --release --example capture_bk_update_fixture -- \
//!         --height 2584711 \
//!         --genesis-bk-set path/to/bk_set.shellnet.json \
//!         --out crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
//! ```
//!
//! `--height` must be the seq_no of the block that emits the rotation event
//! (not the block-before). `--genesis-bk-set` is the JSON snapshot of the
//! BK set at chain height 0 (or any known-good anchor prior to `--height`).
//!
//! The tool re-runs the same code paths the daemon uses at drain time —
//! [`bk_set_at_height`] to reconstruct old_pubkeys,
//! [`parse_bk_set_changes_pub`] to apply the delta — so if the capture
//! itself is broken the parity test will fail at CI time and prompt a
//! recapture rather than shipping bad data.

use std::collections::HashMap;

use anyhow::{bail, Context};
use bridge_gql_fetcher::bk_set_fetcher::{
    bk_set_at_height, load_bk_set_from_config, normalize_bk_set_pubkeys,
    parse_bk_set_changes_pub, BK_CHANGE_VARIANT_ADDED, BK_CHANGE_VARIANT_REMOVED,
};
use bridge_gql_fetcher::gql_client::create_client;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct BkUpdateFixture {
    source: String,
    block_seq_no: u64,
    block_id_be_hex: String,
    leaves_hex: Vec<String>,
    bk_set_update_hex: String,
    old_pubkeys_hex: HashMap<String, String>,
    expected_new_pubkeys_hex: HashMap<String, String>,
}

fn to_hex_map(m: &HashMap<u16, Vec<u8>>) -> HashMap<String, String> {
    m.iter().map(|(k, v)| (k.to_string(), hex::encode(v))).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut target_height: Option<u64> = None;
    let mut genesis_path: Option<String> = None;
    let mut out_path: String =
        "crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json"
            .to_string();
    let mut source_label: String = "shellnet".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--height" => {
                target_height = Some(
                    args.next()
                        .context("--height needs a value")?
                        .parse()
                        .context("--height must be u64")?,
                );
            }
            "--genesis-bk-set" => {
                genesis_path = Some(args.next().context("--genesis-bk-set needs a path")?);
            }
            "--out" => {
                out_path = args.next().context("--out needs a path")?;
            }
            "--source" => {
                source_label = args.next().context("--source needs a label")?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: capture_bk_update_fixture --height N --genesis-bk-set PATH \\\n\
                    \x20\x20                            [--out PATH] [--source LABEL]"
                );
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    let target_height =
        target_height.context("--height is required (bk-update block seq_no)")?;
    let genesis_path = genesis_path.context("--genesis-bk-set is required")?;

    let endpoint = std::env::var("BRIDGE_GQL_ENDPOINT")
        .context("BRIDGE_GQL_ENDPOINT not set")?;
    let client = create_client(&endpoint).context("create_client failed")?;

    // Reconstruct old_pubkeys = fold(genesis, all updates with height < target).
    let genesis = load_bk_set_from_config(&genesis_path)
        .with_context(|| format!("load genesis {}", genesis_path))?;
    println!("genesis: {} signers from {}", genesis.len(), genesis_path);

    let old_pubkeys = if target_height == 0 {
        // Odd case: target IS genesis. Not useful for a rotation fixture but
        // let's fail loudly rather than emit a self-consistent nothing.
        bail!("--height=0 has no rotation to capture");
    } else {
        bk_set_at_height(&client, genesis, target_height - 1)
            .await
            .with_context(|| format!("bk_set_at_height({})", target_height - 1))?
    };
    println!("old_pubkeys: {} signers (folded to height {})", old_pubkeys.len(), target_height - 1);

    // Fetch the target block and locate its bk_set_update_hex.
    let block = client
        .query_proof_block_by_seqno(target_height)
        .await
        .with_context(|| format!("query_proof_block_by_seqno({})", target_height))?;

    let leaves = block.block_merkle_tree_leaves.ok_or_else(|| {
        anyhow::anyhow!(
            "block {} has no block_merkle_tree_leaves — pick a newer block \
             or check node exposes this field",
            target_height,
        )
    })?;

    // The delta blob lives on the bkSetUpdates event, not the block itself —
    // walk the update stream and find the event whose height matches.
    // Uses `bk_set_at_height`-style pagination for robustness on chains with
    // many rotations.
    let (page1, _) = client
        .query_bk_set_updates_paged(target_height, 500, None)
        .await
        .context("query_bk_set_updates_paged")?;
    let hit = page1
        .into_iter()
        .find(|u| u.height == Some(target_height))
        .with_context(|| {
            format!(
                "no bkSetUpdates event at height {} (is this actually a rotation block?)",
                target_height,
            )
        })?;

    println!(
        "target rotation event: height={:?} block_id={} delta={} bytes hex",
        hit.height,
        hit.block_id,
        hit.bk_set_update_hex.len(),
    );

    // Sanity: block_id from bkSetUpdates event and from the block query
    // must agree. If not, the capture is picking the wrong pair.
    if hit.block_id != hex::encode(block.block_id) {
        bail!(
            "bkSetUpdates event.block_id {} != proof_block.block_id {} — \
             pagination or filtering bug in capture",
            hit.block_id,
            hex::encode(block.block_id),
        );
    }

    // Apply the delta to old_pubkeys to derive expected_new_pubkeys — this
    // is what the parity test will independently recompute.
    let blob = hex::decode(&hit.bk_set_update_hex).context("decode bk_set_update_hex")?;
    let changes = parse_bk_set_changes_pub(&blob);
    if changes.is_empty() {
        bail!("parsed 0 changes from delta — parser or blob broken");
    }
    let mut expected_new = old_pubkeys.clone();
    for (variant, idx, pk) in &changes {
        match *variant {
            BK_CHANGE_VARIANT_ADDED => {
                expected_new.insert(*idx, pk.clone());
            }
            BK_CHANGE_VARIANT_REMOVED => {
                expected_new.remove(idx);
            }
            _ => {}
        }
    }
    let expected_new = normalize_bk_set_pubkeys(expected_new).context("normalize expected_new")?;

    let fixture = BkUpdateFixture {
        source: format!("{source_label} @ height {target_height}"),
        block_seq_no: target_height,
        block_id_be_hex: hex::encode(block.block_id),
        leaves_hex: leaves.iter().map(hex::encode).collect(),
        bk_set_update_hex: hit.bk_set_update_hex.clone(),
        old_pubkeys_hex: to_hex_map(&old_pubkeys),
        expected_new_pubkeys_hex: to_hex_map(&expected_new),
    };

    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(&fixture)?;
    std::fs::write(&out_path, json).context("write fixture")?;
    println!("wrote {out_path}");
    Ok(())
}
