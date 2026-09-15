//! Live beacon REST. Ignored by default (`BEACON_URL` required).

use eth_light_client_relayer::{BeaconSource, HttpBeaconSource};

#[tokio::test]
#[ignore = "hits a public beacon; set BEACON_URL"]
async fn live_beacon_finality_update_parses() {
    let url = std::env::var("BEACON_URL")
        .unwrap_or_else(|_| "https://lodestar-mainnet.chainsafe.io".into());
    let src = HttpBeaconSource::new(url).unwrap();
    let u = src.fetch_finality().await.expect("finality_update");
    assert!(u.finalized_slot > 0);
    assert!(u.attested_slot >= u.finalized_slot);
    assert!(u.participation > 0);
}
