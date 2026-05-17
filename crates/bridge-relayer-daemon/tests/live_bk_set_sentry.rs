//! Live integration test for `BkSetSentry` against the AN testnet.
//!
//! `#[ignore]`-gated so the default `cargo test` run stays hermetic
//! (the public testnet at `94.156.178.19:8600` can be flaky). Opt-in
//! via:
//!
//! ```bash
//! cargo test -p bridge-relayer-daemon --test live_bk_set_sentry \
//!     -- --ignored --nocapture
//! ```
//!
//! What this proves:
//!
//! - The sentry's wiring around the lower-layer `BkSetTracker` works end-to-end
//!   against a real node (HTTP, TLS, JSON schema).
//! - Two consecutive ticks produce `Bootstrapped` followed by one of `Quiet {
//!   .. }` or `RotationDetected { .. }` — never another `Bootstrapped`, which
//!   would mean the cache wasn't updated.
//! - The metrics counters advance the way the docs claim.

use std::time::Duration;

use bridge_relayer_daemon::{BkSetSentry, SentryStatus};

const DEFAULT_NODE_URL: &str = "http://94.156.178.19:8600";

fn node_url() -> String {
    std::env::var("AN_NODE_URL").unwrap_or_else(|_| DEFAULT_NODE_URL.to_string())
}

#[tokio::test]
#[ignore]
async fn live_sentry_bootstraps_then_quiet_or_rotation() {
    let _ = tracing_subscriber::fmt::try_init();

    let mut sentry = BkSetSentry::from_node_url(node_url()).expect("sentry construction");

    // First tick: Bootstrapped (cold start).
    let first = sentry.tick().await.expect("first tick");
    let (bs_seq, bs_count) = match first {
        SentryStatus::Bootstrapped {
            observed_seq_no,
            bk_count,
        } => (observed_seq_no, bk_count),
        other => panic!("expected Bootstrapped, got {other:?}"),
    };
    assert!(bs_count > 0, "live committee must be non-empty");

    let m1 = sentry.metrics();
    assert_eq!(m1.total_ticks, 1);
    assert_eq!(m1.successful_ticks, 1);
    assert_eq!(m1.last_observed_seq_no, bs_seq);
    assert_eq!(m1.rotations_observed, 0);

    println!("OK bootstrap: seq_no={bs_seq} bk_count={bs_count}");

    // Give the node ~half a second to advance, then poll again.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let second = sentry.tick().await.expect("second tick");
    match &second {
        SentryStatus::Bootstrapped {
            ..
        } => panic!("Bootstrapped must not repeat — sentry cache wasn't updated"),
        SentryStatus::Quiet {
            observed_seq_no, ..
        } => {
            assert!(*observed_seq_no >= bs_seq);
            println!("OK second tick: Quiet at seq_no={observed_seq_no}");
        },
        SentryStatus::RotationDetected {
            old_seq_no,
            new_seq_no,
            delta,
        } => {
            // Extremely unlikely to fire within 500 ms on the public
            // testnet, but if it does that's a real protocol event
            // — log it for visibility.
            println!(
                "OK second tick: RotationDetected ({old_seq_no}→{new_seq_no}, +{} -{} mut={})",
                delta.added.len(),
                delta.removed.len(),
                delta.pubkey_mutations.len(),
            );
        },
    }

    let m2 = sentry.metrics();
    assert_eq!(m2.total_ticks, 2);
    assert_eq!(m2.successful_ticks, 2);
    assert!(m2.last_observed_seq_no >= m1.last_observed_seq_no);

    assert!(sentry.latest().is_some(), "cache populated after polls");
}
