//! Opt-in live AN connectivity test.
//!
//! `#[ignore]`-gated so the default `cargo test` (and CI) never reaches out to
//! a network. Run it explicitly for an E2E check against a reachable AN node:
//!
//! ```bash
//! # Against the local 5-node cluster (../acki-nacki/nock docker-compose):
//! cargo test -p deposit-relayer-daemon --test live_an_preflight -- --ignored --nocapture
//!
//! # Against a different node:
//! AN_NODE_URL=http://94.156.178.19:8600 \
//!   cargo test -p deposit-relayer-daemon --test live_an_preflight -- --ignored --nocapture
//! ```
//!
//! Defaults to the local-cluster Node0 (`DEFAULT_LOCAL_AN_NODE_URL`) so the
//! common E2E path needs no env var.

use deposit_relayer_daemon::{AnConfig, DEFAULT_LOCAL_AN_NODE_URL};

#[tokio::test]
#[ignore = "hits a live AN node; run with --ignored against a reachable cluster"]
async fn live_an_preflight_succeeds() {
    let node_url =
        std::env::var("AN_NODE_URL").unwrap_or_else(|_| DEFAULT_LOCAL_AN_NODE_URL.to_string());

    let cfg = AnConfig::from_node_url(&node_url);
    let pf = cfg
        .preflight()
        .await
        .unwrap_or_else(|e| panic!("AN preflight against {node_url} failed: {e}"));

    println!(
        "AN preflight OK: node={} seq_no={} bk_count={} future_bk_count={}",
        pf.node_url, pf.seq_no, pf.bk_count, pf.future_bk_count
    );
    assert_eq!(pf.node_url, node_url.trim_end_matches('/'));
    // A healthy chain always has at least one active Block Keeper.
    assert!(pf.bk_count > 0, "expected a non-empty BK set");
}
