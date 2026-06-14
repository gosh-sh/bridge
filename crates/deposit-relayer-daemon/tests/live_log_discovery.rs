//! Live check that the production `EthLogSource` discovery path (eth_getLogs →
//! receipt log-index mapping) finds the documented Sepolia deposit.
//!
//! Run with:
//!   SEPOLIA_RPC_URL=... BRIDGE_DEPLOY_BLOCK=11025180 \
//!     cargo test -p deposit-relayer-daemon --test live_log_discovery -- --ignored --nocapture

use std::str::FromStr;

use alloy::{primitives::Address, providers::ProviderBuilder};
use deposit_relayer_daemon::{resolve_from_block, DepositSource, EthLogSource};

const SEPOLIA_BRIDGE: &str = "0x99c37fb75326ae6953ebbbdcd261ec331df4ce82";
const DEPOSIT_TX: &str = "0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf";
const EXPECTED_RECEIPT_LOG_INDEX: u64 = 2;

#[tokio::test]
#[ignore = "hits live Sepolia RPC; exercises production eth_getLogs discovery"]
async fn live_eth_log_source_finds_deposit_id0() {
    let rpc_url = std::env::var("SEPOLIA_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .expect("set SEPOLIA_RPC_URL or RPC_URL");
    let from_block = resolve_from_block(0);
    assert_ne!(
        from_block, 0,
        "set BRIDGE_DEPLOY_BLOCK (e.g. 11025180 for the shellnet Sepolia bridge)",
    );

    let provider = ProviderBuilder::new()
        .connect_http(rpc_url.parse().expect("valid RPC URL"));
    let bridge = Address::from_str(SEPOLIA_BRIDGE).unwrap();
    let source = EthLogSource::new(provider, bridge, from_block, 12);

    let event = source
        .fetch(0)
        .await
        .expect("fetch should not error")
        .expect("depositId=0 should be confirmed on Sepolia");

    assert_eq!(format!("{:#x}", event.tx_hash), DEPOSIT_TX);
    assert_eq!(
        event.log_index, EXPECTED_RECEIPT_LOG_INDEX,
        "receipt-local log index must match deposit-prover expectation",
    );
}
