//! Live check that the production `EthLogSource` discovery path (eth_getLogs →
//! receipt log-index mapping) finds a deposit from a fresh Sepolia deployment.
//!
//! Run with:
//!   SEPOLIA_RPC_URL=... BRIDGE_DEPLOY_BLOCK=... BRIDGE_ADDRESS=... \
//!     DEPOSIT_TX_HASH=... EXPECTED_RECEIPT_LOG_INDEX=... \
//!     cargo test -p deposit-relayer-daemon --test live_log_discovery --
//! --ignored --nocapture

use std::str::FromStr;

use alloy::{
    primitives::{Address, B256},
    providers::ProviderBuilder,
};
use deposit_relayer_daemon::{resolve_from_block, DepositSource, EthLogSource};

#[tokio::test]
#[ignore = "hits live Sepolia RPC; exercises production eth_getLogs discovery"]
async fn live_eth_log_source_finds_a_deposit() {
    let rpc_url = std::env::var("SEPOLIA_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .expect("set SEPOLIA_RPC_URL or RPC_URL");
    let from_block = resolve_from_block(0);
    assert_ne!(
        from_block, 0,
        "set BRIDGE_DEPLOY_BLOCK for the fresh deployment",
    );

    let provider = ProviderBuilder::new().connect_http(rpc_url.parse().expect("valid RPC URL"));
    let bridge =
        Address::from_str(&std::env::var("BRIDGE_ADDRESS").expect("set BRIDGE_ADDRESS")).unwrap();
    let tx =
        B256::from_str(&std::env::var("DEPOSIT_TX_HASH").expect("set DEPOSIT_TX_HASH")).unwrap();
    let log_index: u64 = std::env::var("EXPECTED_RECEIPT_LOG_INDEX")
        .expect("set EXPECTED_RECEIPT_LOG_INDEX")
        .parse()
        .unwrap();
    let deposit_id: u64 = std::env::var("DEPOSIT_ID")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .unwrap();
    let source = EthLogSource::new(provider, bridge, from_block, 12);

    let event = source
        .fetch(deposit_id)
        .await
        .expect("fetch should not error")
        .expect("the deposit should be confirmed on Sepolia");

    assert_eq!(event.tx_hash, tx);
    assert_eq!(
        event.log_index, log_index,
        "receipt-local log index must match deposit-prover expectation",
    );
}
