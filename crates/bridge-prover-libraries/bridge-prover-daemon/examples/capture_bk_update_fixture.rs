//! Capture a live-chain BK-set-update fixture for
//! [`bridge_prover_lib::tests::parity_bk_update`].
//!
//! On the endpoint given by `BRIDGE_GQL_ENDPOINT`, builds the BK set before
//! and after the rotation at `--height` from GraphQL with
//! [`build_bk_set_with_retry`] (`old_pubkeys` checked against leaf 2 of the
//! rotation block, `expected_new_pubkeys` against leaf 3), fetches the
//! block's 16-leaf tree and the rotation's delta blob, and writes the JSON
//! fixture the parity test consumes.
//!
//! Usage:
//!
//! ```sh
//! BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
//!     cargo run --release --example capture_bk_update_fixture -- \
//!         --height 2584711 \
//!         --out crates/bridge-prover-libraries/bridge-prover-lib/tests/fixtures/bk_update_shellnet.json
//! ```
//!
//! `--height` must be the seq_no of the block that emits the rotation event
//! (not the block-before). The server must serve `blockchain.latestBkSet`
//! (gql-server >= 1.5.0) and still hold the `bkSetUpdates` rows between its
//! snapshot and `--height`.
//!
//! The delta is applied to `old_pubkeys` with the code the daemon uses
//! ([`parse_changes`] and [`apply_block_changes`], node order) and must give
//! `expected_new_pubkeys`, so a broken capture fails here rather than
//! shipping bad data.

use std::collections::HashMap;

use anyhow::{bail, Context};
use bridge_gql_fetcher::gql_client::{create_client, DEFAULT_THREAD_ID_HEX};
use bridge_prover_lib::{
    bk_set_history::{build_bk_set_with_retry, BuildPolicy},
    bk_set_replay::{apply_block_changes, parse_changes, BkTarget, FinalCheck},
};
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
            "--out" => {
                out_path = args.next().context("--out needs a path")?;
            }
            "--source" => {
                source_label = args.next().context("--source needs a label")?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: capture_bk_update_fixture --height N [--out PATH] [--source LABEL]"
                );
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    let target_height =
        target_height.context("--height is required (bk-update block seq_no)")?;
    if target_height == 0 {
        // Height 0 is the genesis, not a rotation: fail loudly rather than
        // emit a self-consistent nothing.
        bail!("--height=0 has no rotation to capture");
    }

    let endpoint = std::env::var("BRIDGE_GQL_ENDPOINT")
        .context("BRIDGE_GQL_ENDPOINT not set")?;
    let client = create_client(&endpoint).context("create_client failed")?;

    // The set before the rotation signs the rotation block (leaf 2); the set
    // after it is leaf 3 of the same block.
    let policy = BuildPolicy::default();
    let old_pubkeys = build_bk_set_with_retry(
        &client,
        &BkTarget {
            height: target_height - 1,
            check: FinalCheck::L2Of(target_height),
        },
        &policy,
    )
    .await
    .with_context(|| format!("build the BK set before the rotation at {target_height}"))?
    .pubkeys;
    let expected_new = build_bk_set_with_retry(
        &client,
        &BkTarget {
            height: target_height,
            check: FinalCheck::L3Of(target_height),
        },
        &policy,
    )
    .await
    .with_context(|| format!("build the BK set after the rotation at {target_height}"))?
    .pubkeys;
    println!(
        "old_pubkeys: {} signers, expected_new_pubkeys: {} signers (rotation at {})",
        old_pubkeys.len(),
        expected_new.len(),
        target_height,
    );

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

    // The delta blob lives on the bkSetUpdates row, not the block itself —
    // ask for the rows at exactly the target height.
    let (rows, _) = client
        .query_bk_set_updates_range(
            DEFAULT_THREAD_ID_HEX,
            Some(target_height),
            Some(target_height),
            10,
            None,
        )
        .await
        .context("query_bk_set_updates_range")?;
    let hit = rows
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

    // Apply the delta to old_pubkeys in node order — what the parity test
    // recomputes — and require the set built after the rotation.
    let changes = parse_changes(&hit.bk_set_update_hex).context("parse the rotation's delta")?;
    let mut applied = old_pubkeys.clone();
    apply_block_changes(&mut applied, &changes).context("apply the rotation's delta")?;
    if applied != expected_new {
        bail!(
            "the delta at {} applied to the set before it does not give the set after it — parser \
             or blob broken",
            target_height,
        );
    }

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
