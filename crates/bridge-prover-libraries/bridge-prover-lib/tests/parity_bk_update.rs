//! Cross-crate parity test for the BK-set-update path.
//!
//! Re-executes the four checks from
//! [`bridge_prover_lib::live_driver::bk_update::drive_next_bk_update`]
//! against a captured live-chain fixture, so a regression in any of:
//!   * `bridge-poseidon::compute_bk_set_poseidon` (schema drift),
//!   * `bridge-gql-fetcher::bk_set_fetcher::parse_bk_set_changes_pub` (blob layout drift),
//!   * `bridge-gql-fetcher::bk_set_fetcher::normalize_bk_set_pubkeys` (compression drift),
//!   * `bridge-prover-lib::block_id_tree::BlockIdMerkleTree` (leaf order / fold direction),
//!
//! fires at `cargo test` time on CI instead of at first live drain on a running
//! deploy.
//!
//! Fixture format is captured by
//! `bridge-prover-daemon/examples/capture_bk_update_fixture.rs`. If the file at
//! `tests/fixtures/bk_update_shellnet.json` is absent, the test is a no-op
//! (with a printed hint) — no CI failure on a fresh checkout that has never
//! run the capture. Land a fixture once, and the test starts guarding drift.
//!
//! Rationale: the value here is drift detection between our modules and what
//! the AN node produced. A synthetic fixture would not exercise the node-shape
//! invariants (real bincode discriminants, real 96B→48B compression, real
//! Poseidon padding row), so we deliberately require a captured live sample.

use std::collections::HashMap;
use std::path::PathBuf;

use bridge_gql_fetcher::bk_set_fetcher::{
    normalize_bk_set_pubkeys, parse_bk_set_changes_pub, BK_CHANGE_VARIANT_ADDED,
    BK_CHANGE_VARIANT_REMOVED,
};
use bridge_poseidon as poseidon;
use bridge_prover_lib::block_id_tree::{BlockIdMerkleTree, BLOCK_ID_TREE_LEAF_COUNT};
use serde::{Deserialize, Serialize};

/// On-disk schema. Kept as plain hex strings (not `[u8;32]`) so hand-edits and
/// diff review stay readable; the test parses to bytes.
#[derive(Debug, Serialize, Deserialize)]
pub struct BkUpdateFixture {
    /// Human context — chain name / capture timestamp.
    pub source: String,
    /// The bk-update event's block seq_no on the source chain.
    pub block_seq_no: u64,
    /// `block_id` reported by GQL for the bk-update block (hex, 32B).
    pub block_id_be_hex: String,
    /// All 16 SHA-256 leaves of the block-id tree, in canonical order (hex, 32B each).
    pub leaves_hex: Vec<String>,
    /// The `bk_set_update_hex` delta blob AN emits on the bkSetUpdates stream.
    pub bk_set_update_hex: String,
    /// The BK set active immediately BEFORE this rotation (signer_idx -> pubkey hex).
    /// Stored in the same 48B compressed form the prover uses at runtime.
    pub old_pubkeys_hex: HashMap<String, String>,
    /// The BK set the capture tool derived by applying the delta to `old_pubkeys_hex`.
    /// The test recomputes this from scratch and asserts equality — so this field
    /// pins the capture tool honest.
    pub expected_new_pubkeys_hex: HashMap<String, String>,
}

fn decode_leaves(hex_leaves: &[String]) -> [[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT] {
    assert_eq!(
        hex_leaves.len(),
        BLOCK_ID_TREE_LEAF_COUNT,
        "fixture must have exactly {} leaves",
        BLOCK_ID_TREE_LEAF_COUNT
    );
    let mut out = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
    for (i, s) in hex_leaves.iter().enumerate() {
        let raw = hex::decode(s).unwrap_or_else(|e| panic!("leaf[{i}] not hex: {e}"));
        out[i] = raw
            .try_into()
            .unwrap_or_else(|v: Vec<u8>| panic!("leaf[{i}] len {} != 32", v.len()));
    }
    out
}

fn decode_pubkeys(map: &HashMap<String, String>) -> HashMap<u16, Vec<u8>> {
    let mut out = HashMap::with_capacity(map.len());
    for (k, v) in map {
        let idx: u16 = k.parse().unwrap_or_else(|e| panic!("signer idx {k} not u16: {e}"));
        let bytes = hex::decode(v).unwrap_or_else(|e| panic!("pubkey {k} not hex: {e}"));
        out.insert(idx, bytes);
    }
    out
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bk_update_shellnet.json")
}

#[test]
fn bk_update_parity_against_captured_fixture() {
    let path = fixture_path();
    if !path.exists() {
        eprintln!(
            "SKIP: parity fixture {} not present.\n\
             To capture: BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \\\n\
             \x20\x20cargo run --release --example capture_bk_update_fixture -- \\\n\
             \x20\x20  --height <bk-update block seqno> \\\n\
             \x20\x20  --genesis-bk-set path/to/bk_set.shellnet.json \\\n\
             \x20\x20  --out {}",
            path.display(),
            path.display(),
        );
        return;
    }

    let raw = std::fs::read_to_string(&path).expect("read fixture");
    let fx: BkUpdateFixture = serde_json::from_str(&raw).expect("parse fixture");
    eprintln!(
        "loaded fixture: source={} block_seq_no={}",
        fx.source, fx.block_seq_no
    );

    // (a) Tree fold parity — mirrors bk_update.rs:81..98 guard.
    let leaves = decode_leaves(&fx.leaves_hex);
    let block_id_be: [u8; 32] = hex::decode(&fx.block_id_be_hex)
        .expect("block_id_be_hex")
        .try_into()
        .expect("block_id_be len");
    let tree = BlockIdMerkleTree::from_leaves(leaves);
    assert_eq!(
        tree.root, block_id_be,
        "BlockIdMerkleTree fold(leaves) != captured block_id — \
         leaf ordering or SHA-256 combine direction has drifted",
    );

    // (b) L2 = Poseidon(old_pubkeys) — mirrors bk_update.rs:106..114 cross-check.
    let old_pubkeys = decode_pubkeys(&fx.old_pubkeys_hex);
    let old_pubkeys = normalize_bk_set_pubkeys(old_pubkeys)
        .expect("normalize old_pubkeys — capture must store 48/96B keys");
    let (_, l2_expected) = poseidon::compute_bk_set_poseidon(&old_pubkeys);
    assert_eq!(
        l2_expected, tree.leaves[2],
        "compute_bk_set_poseidon(old_pubkeys) != leaves[2] — bridge-poseidon \
         drifted from the AN node's BK-set Poseidon schema",
    );

    // (c) Apply delta and check we reach `expected_new_pubkeys_hex`.
    // Mirrors bk_update.rs:118..149 delta application.
    let blob = hex::decode(&fx.bk_set_update_hex).expect("bk_set_update_hex not hex");
    let changes = parse_bk_set_changes_pub(&blob);
    assert!(
        !changes.is_empty(),
        "parse_bk_set_changes_pub returned 0 changes — parser drifted from \
         AN node's bincode layout for BlockKeeperSetChange",
    );
    let mut new_pubkeys = old_pubkeys.clone();
    for (variant, idx, pk) in &changes {
        match *variant {
            BK_CHANGE_VARIANT_ADDED => {
                new_pubkeys.insert(*idx, pk.clone());
            }
            BK_CHANGE_VARIANT_REMOVED => {
                new_pubkeys.remove(idx);
            }
            _ => {}
        }
    }
    let new_pubkeys =
        normalize_bk_set_pubkeys(new_pubkeys).expect("normalize new_pubkeys after delta");
    let expected_new = decode_pubkeys(&fx.expected_new_pubkeys_hex);
    let expected_new = normalize_bk_set_pubkeys(expected_new).expect("normalize expected_new");
    assert_eq!(
        new_pubkeys, expected_new,
        "delta application diverged from capture-time expected new pubkeys \
         — parse_bk_set_changes_pub semantics or normalization drifted",
    );

    // (d) L3 = Poseidon(new_pubkeys) — mirrors bk_update.rs:150..158 guard.
    let (_, l3_expected) = poseidon::compute_bk_set_poseidon(&new_pubkeys);
    assert_eq!(
        l3_expected, tree.leaves[3],
        "compute_bk_set_poseidon(new_pubkeys) != leaves[3] — schema drift \
         end-to-end (would panic the daemon at first live drain)",
    );

    eprintln!("all 4 parity checks passed against captured fixture");
}
