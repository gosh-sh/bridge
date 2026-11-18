//! Read the bridge's mint record for a known deposit on a live deployment.

use std::sync::Arc;

use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use alloy::primitives::{Address, U256};
use deposit_relayer_daemon::{
    bridge_abi,
    submitter::{AnInterfaceSubmitter, AnSubmitConfig, DepositStatus},
    voucher::DepositIdentity,
};
use tvm_client::crypto::KeyPair;

#[tokio::test]
#[ignore = "needs AN_GRAPHQL_URL, AN_TOKEN_BRIDGE, BRIDGE_ADDRESS, CHAIN_ID and a minted DEPOSIT_ID"]
async fn live_bridge_reports_the_minted_deposit() {
    let env = |name: &str| std::env::var(name).expect(name);
    let client = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![env("AN_GRAPHQL_URL")],
        keys: KeyPair {
            public: "00".repeat(32),
            secret: "00".repeat(32),
        },
        bridge_abi: bridge_abi(),
    })
    .unwrap();
    let sub = AnInterfaceSubmitter::new(Arc::new(client), AnSubmitConfig {
        from: String::new(),
        token_bridge: env("AN_TOKEN_BRIDGE"),
        confirm_timeout_secs: 1,
    });
    let address: Address = env("BRIDGE_ADDRESS").parse().unwrap();
    let id = DepositIdentity {
        deposit_id: env("DEPOSIT_ID").parse().unwrap(),
        contract_addr: U256::from_be_slice(address.as_slice()),
        chain_id: env("CHAIN_ID").parse().unwrap(),
    };
    assert_eq!(sub.deposit_status(id).await.unwrap(), DepositStatus::Minted);
}
