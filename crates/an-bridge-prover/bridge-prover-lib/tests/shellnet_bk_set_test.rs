//! Test BK set extraction from shellnet via GraphQL bkSetUpdates.
//!
//! **DISABLED (2026-07-22)** — targets the removed `fetch_bk_set` helper,
//! which was architecturally broken (replayed the `bkSetUpdates` delta log
//! from ∅ but AN does not emit genesis as a synthetic `Added` event, so the
//! reconstruction was wrong on rotating chains and empty on fresh ones).
//! Re-enable once `bk_set_at_height(genesis, target_height)` lands and this
//! test is rewritten to fold-forward from a known genesis snapshot.

#[cfg(any())]
#[tokio::test]
async fn test_shellnet_bk_set_extraction() {
    let gql = bridge_prover_lib::gql_client::create_client("https://shellnet.ackinacki.org")
        .expect("failed to create shellnet client");

    println!("Querying shellnet bkSetUpdates...");
    match bridge_prover_lib::bk_set_fetcher::fetch_bk_set(&gql).await {
        Ok(bk_set) => {
            println!("BK set extracted: {} signers", bk_set.len());
            let mut keys: Vec<u16> = bk_set.keys().cloned().collect();
            keys.sort();
            for idx in &keys {
                println!(
                    "  signer {}: {} ({}B)",
                    idx,
                    hex::encode(&bk_set[idx][..8]),
                    bk_set[idx].len()
                );
            }
            assert!(bk_set.len() >= 3, "expected at least 3 signers");
            for pk in bk_set.values() {
                assert_eq!(pk.len(), 96, "pubkey should be 96 bytes (uncompressed)");
            }
            println!("\nShellnet BK set extraction: PASSED");
        }
        Err(e) => {
            println!("BK set extraction failed: {}", e);
            println!("This may be expected if the initial BK set was at genesis.");
            println!("Falling back would require a bk_set.json config for shellnet.");
        }
    }
}
