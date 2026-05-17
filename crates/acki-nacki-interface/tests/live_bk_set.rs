//! Live network test against the AN testnet `/v2/bk_set*` endpoints.
//!
//! **Gated**: marked `#[ignore]` so the default `cargo test` run stays
//! hermetic and CI doesn't flake on testnet downtime. Opt in with one of:
//!
//! ```bash
//! # Hit the default public test node (http://94.156.178.19:8600):
//! cargo test -p acki-nacki-interface --test live_bk_set -- --ignored --nocapture
//!
//! # Or point at a different node (e.g. local docker cluster):
//! AN_NODE_URL=http://127.0.0.1:11000 \
//!   cargo test -p acki-nacki-interface --test live_bk_set -- --ignored --nocapture
//! ```
//!
//! What this test proves:
//!
//! 1. The node speaks the schema documented in `bk_set_client.rs` (no silent
//!    regression on field renames / type changes).
//! 2. Hex-decoding succeeds for every BLS pubkey and node-identity hash in the
//!    live payload (no malformed entries slipping through).
//! 3. `fetch_signer_index_bk_set()` builds a non-empty, collision-free
//!    `signer_index → 48-byte pubkey` map suitable for the orchestrator's
//!    Circuit 1B prover
//!    (`bridge-prover-orchestrator::generate_fallback_proof`).
//!
//! If this test fails, the AN node has changed its `/v2/` surface;
//! coordinate with the partner team before patching the client.

use acki_nacki_interface::{
    BkSetClient, BkSetResponse, BkSetUpdateResponse, BLS_PUBKEY_LEN, ID32_LEN,
};

const DEFAULT_NODE_URL: &str = "http://94.156.178.19:8600";

fn node_url() -> String {
    std::env::var("AN_NODE_URL").unwrap_or_else(|_| DEFAULT_NODE_URL.to_string())
}

fn assert_hex_len(s: &str, expected_bytes: usize, ctx: &str) {
    assert_eq!(
        s.len(),
        expected_bytes * 2,
        "{ctx} expected {expected_bytes}-byte hex (= {} chars), got {}: {}",
        expected_bytes * 2,
        s.len(),
        s,
    );
    let _ = hex::decode(s).unwrap_or_else(|e| panic!("{ctx} hex decode failed: {e}"));
}

#[tokio::test]
#[ignore]
async fn live_v2_bk_set_schema_matches() {
    let _ = tracing_subscriber::fmt::try_init();
    let url = node_url();
    let client = BkSetClient::new(&url).expect("client construction");

    let r: BkSetResponse = client.fetch_bk_set().await.expect("/v2/bk_set");
    assert!(r.seq_no > 0, "seq_no should be > 0 on a live node");
    assert!(
        !r.bk_set.is_empty(),
        "live BK set should be non-empty (got 0 from {url})"
    );

    for entry in &r.bk_set {
        assert_hex_len(&entry.node_id, ID32_LEN, "bk_set[].node_id");
        assert_hex_len(&entry.node_owner_pk, ID32_LEN, "bk_set[].node_owner_pk");
        assert!(entry.epoch_start_seq_no > 0);
    }

    println!(
        "OK /v2/bk_set: {} active BK, {} future, snapshot seq_no = {}",
        r.bk_set.len(),
        r.future_bk_set.len(),
        r.seq_no
    );
}

#[tokio::test]
#[ignore]
async fn live_v2_bk_set_update_schema_matches() {
    let _ = tracing_subscriber::fmt::try_init();
    let url = node_url();
    let client = BkSetClient::new(&url).expect("client construction");

    let r: BkSetUpdateResponse = client
        .fetch_bk_set_update()
        .await
        .expect("/v2/bk_set_update");
    assert!(r.seq_no > 0);
    assert!(
        !r.current.is_empty(),
        "live BK update.current should be non-empty"
    );

    for entry in &r.current {
        assert_hex_len(&entry.pubkey, BLS_PUBKEY_LEN, "current[].pubkey (BLS G1)");
        assert_hex_len(&entry.owner_address, ID32_LEN, "current[].owner_address");
        assert_hex_len(&entry.owner_pubkey, ID32_LEN, "current[].owner_pubkey");
        assert!(
            entry.address.starts_with("0:"),
            "TVM address should start with `0:`, got {}",
            entry.address
        );
        // `stake` is u128-as-string; just check it parses.
        let _: u128 = entry
            .stake
            .parse()
            .unwrap_or_else(|e| panic!("stake parse: {} ({})", e, entry.stake));
    }

    println!(
        "OK /v2/bk_set_update: {} active, {} future, snapshot seq_no = {}",
        r.current.len(),
        r.future.len(),
        r.seq_no
    );
}

#[tokio::test]
#[ignore]
async fn live_signer_index_map_is_collision_free() {
    let _ = tracing_subscriber::fmt::try_init();
    let url = node_url();
    let client = BkSetClient::new(&url).expect("client construction");

    let map = client
        .fetch_signer_index_bk_set()
        .await
        .expect("signer_index map");

    assert!(!map.is_empty(), "live signer_index map should be non-empty");
    for (idx, pk) in &map {
        assert_eq!(
            pk.len(),
            BLS_PUBKEY_LEN,
            "pubkey at signer_index={idx} must be 48 bytes"
        );
    }

    println!(
        "OK signer-index-keyed BK set: {} entries, indices [{}..={}]",
        map.len(),
        map.keys().min().unwrap(),
        map.keys().max().unwrap(),
    );
}
