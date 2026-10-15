//! Live read of the two transactions a `finalizeDeposit` sets off, against a
//! deposit finalized on shellnet: the voucher deploy and its `confirmDeposit`.
//!
//! ```text
//! cargo test --test live_finalize_children -- --ignored
//! ```
//!
//! `AN_GRAPHQL_URL` overrides the endpoint (default shellnet).

use acki_nacki_interface::{IAckiNacki, TvmAckiNacki, TvmClientConfig};
use tvm_client::crypto::KeyPair;

const BRIDGE_ABI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json"
);

fn hash(hex_str: &str) -> [u8; 32] {
    let mut h = [0u8; 32];
    hex::decode_to_slice(hex_str, &mut h).unwrap();
    h
}

#[tokio::test]
#[ignore = "needs the shellnet GraphQL endpoint"]
async fn live_finalize_deposit_children_resolve() {
    let url = std::env::var("AN_GRAPHQL_URL")
        .unwrap_or_else(|_| "https://shellnet.ackinacki.org/graphql".to_string());
    let client = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![url],
        // Read-only: nothing is signed.
        keys: KeyPair {
            public: "00".repeat(32),
            secret: "00".repeat(32),
        },
        bridge_abi: TvmAckiNacki::load_abi(BRIDGE_ABI).unwrap(),
    })
    .unwrap();

    let finalize = hash("4b722bbe924c1d2e0e5450da89160c053ea3a7ee32c8ef42d7709a164885084e");
    let voucher = client.get_internal_children(&finalize).await.unwrap();
    assert_eq!(voucher.len(), 1);
    assert_eq!(
        voucher[0].dst,
        "0:ffd5c4e7770b73e67ad8a44c828c0d2ee2e642cbbff53096ffd0c68ed0f9a80e"
    );
    let deploy = voucher[0].outcome.clone().expect("voucher deploy resolved");
    assert!(deploy.is_success());
    assert_eq!(
        deploy.tx_hash,
        hash("cc1acf80002f680f159fa993f27c6cb9c8759f69aaff76e6a72c1c97ef1a69e8")
    );

    let confirm = client.get_internal_children(&deploy.tx_hash).await.unwrap();
    let mint = confirm[0].outcome.clone().expect("confirmDeposit resolved");
    assert!(mint.is_success());
    assert_eq!(
        mint.tx_hash,
        hash("6ed83e086ca34e49f2548f562934ad1a8c6c8b813415af14d6e36e831b8a760f")
    );
}
