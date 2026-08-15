//! TD-41 / DEP-MAINNET-ID — daemon startup rejects Ethereum L1 `eth_chainId = 1`.

use deposit_relayer_daemon::{
    DepositDeploymentProfile, ensure_supported_chain_id_value,
    validate_chain_for_deployment_profile,
};

const ETHEREUM_MAINNET: u64 = 1;

/// Same gate as `run_daemon` → `ensure_supported_rpc_chain` before the relayer loop.
#[test]
fn td_41_preflight_rejects_mainnet_chain_id_before_tick() {
    let err = ensure_supported_chain_id_value(ETHEREUM_MAINNET, None).unwrap_err();
    assert!(
        err.to_string().contains("not a supported deposit chain"),
        "TD-41: chainId=1 must fail preflight: {err}"
    );
    assert!(err.to_string().contains("1"));
}

#[test]
fn td_41_preflight_rejects_mainnet_even_with_expect_chain_id() {
    let err = ensure_supported_chain_id_value(ETHEREUM_MAINNET, Some(ETHEREUM_MAINNET))
        .unwrap_err();
    assert!(
        err.to_string().contains("not a supported deposit chain"),
        "expect-chain-id cannot whitelist mainnet L1: {err}"
    );
}

#[test]
fn td_41_prod_profile_rejects_mainnet_chain_id() {
    let err = validate_chain_for_deployment_profile(
        ETHEREUM_MAINNET,
        DepositDeploymentProfile::Production,
    )
    .unwrap_err();
    assert!(
        err.contains("PRODUCTION_DEPOSIT_CHAIN_IDS") || err.contains("not supported"),
        "TD-41 prod ops gate: {err}"
    );
}

#[test]
fn td_41_shellnet_profile_rejects_mainnet_chain_id() {
    let err = validate_chain_for_deployment_profile(
        ETHEREUM_MAINNET,
        DepositDeploymentProfile::ShellnetOrDev,
    )
    .unwrap_err();
    assert!(err.contains("not supported"), "TD-41 shellnet gate: {err}");
}

/// Regression: TD-30 already covered chainId=1; TD-41 documents dedicated PoC.
#[test]
fn td_41_cross_ref_td_30_same_preflight_error_shape() {
    let td41 = ensure_supported_chain_id_value(ETHEREUM_MAINNET, None).unwrap_err().to_string();
    assert!(td41.contains("not a supported deposit chain"));
    assert!(td41.contains("supported:"));
}
