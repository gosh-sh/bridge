//! Live voucher pre-check against shellnet: reads `_depositVoucherCode` from
//! the deployed bridge, derives the voucher of a deposit finalized there, and
//! finds it active.
//!
//! ```text
//! cargo test --test live_voucher_precheck -- --ignored
//! ```
//!
//! `AN_GRAPHQL_URL` overrides the endpoint (default shellnet).

use std::sync::Arc;

use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use deposit_relayer_daemon::{
    submitter::{AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter},
    types::DepositEvent,
};
use tvm_client::crypto::KeyPair;

const BRIDGE_ABI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json"
);

#[tokio::test]
#[ignore = "needs the shellnet GraphQL endpoint"]
async fn live_voucher_precheck_finds_a_finalized_deposit() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("deposit_relayer_daemon=debug")
        .try_init();
    let url = std::env::var("AN_GRAPHQL_URL")
        .unwrap_or_else(|_| "https://shellnet.ackinacki.org/graphql".to_string());
    let abi = TvmAckiNacki::load_abi(BRIDGE_ABI).unwrap();
    let client = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![url],
        // Read-only: nothing is signed.
        keys: KeyPair {
            public: "00".repeat(32),
            secret: "00".repeat(32),
        },
        bridge_abi: abi.clone(),
    })
    .unwrap();
    let bridge = "1a".repeat(32);
    let sub = AnInterfaceSubmitter::new(Arc::new(client), AnSubmitConfig {
        from: format!("{bridge}::{}", "ff".repeat(32)),
        token_bridge: format!("{bridge}::{bridge}"),
        confirm_timeout_secs: 1,
    })
    .with_voucher_check(abi)
    .unwrap();

    // Finalized on shellnet by bridge tx 4b722bbe… (voucher 0:ffd5c4e7…).
    let finalized = DepositEvent {
        dapp_id: alloy::primitives::U256::ZERO,
        deposit_id: 1,
        sender: Default::default(),
        amount: Default::default(),
        an_account: Default::default(),
        timestamp: Default::default(),
        tx_hash: Default::default(),
        log_index: 0,
        block_number: 0,
        block_hash: Default::default(),
        source_contract: "0x32b9e87acaa1ad7d61a81f93dd9d525f64ff4f38"
            .parse()
            .unwrap(),
        source_chain_id: 11_155_111,
    };
    assert!(sub.is_finalized(&finalized).await.unwrap());

    let never = DepositEvent {
        deposit_id: u64::MAX,
        ..finalized
    };
    assert!(!sub.is_finalized(&never).await.unwrap());
}
