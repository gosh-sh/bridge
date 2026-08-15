//! TD-41 / DEP-MAINNET-ID — Ethereum L1 mainnet (`chainId = 1`) is not a deposit source.

use deposit_chain_ids::{
    DepositDeploymentProfile, PRODUCTION_DEPOSIT_CHAIN_IDS, SUPPORTED_DEPOSIT_CHAIN_IDS,
    is_production_deposit_chain, is_supported_deposit_chain,
    validate_chain_for_deployment_profile,
};

const ETHEREUM_MAINNET: u64 = 1;

#[test]
fn td_41_mainnet_not_in_supported_allowlist() {
    assert!(
        !SUPPORTED_DEPOSIT_CHAIN_IDS.contains(&ETHEREUM_MAINNET),
        "Ethereum L1 mainnet must not be in SUPPORTED_DEPOSIT_CHAIN_IDS"
    );
    assert!(!is_supported_deposit_chain(ETHEREUM_MAINNET));
}

#[test]
fn td_41_mainnet_not_in_production_allowlist() {
    assert!(
        !PRODUCTION_DEPOSIT_CHAIN_IDS.contains(&ETHEREUM_MAINNET),
        "Ethereum L1 mainnet must not be in PRODUCTION_DEPOSIT_CHAIN_IDS"
    );
    assert!(!is_production_deposit_chain(ETHEREUM_MAINNET));
}

#[test]
fn td_41_prod_profile_rejects_mainnet_chain_id() {
    let err = validate_chain_for_deployment_profile(
        ETHEREUM_MAINNET,
        DepositDeploymentProfile::Production,
    )
    .unwrap_err();
    assert!(
        err.contains("not in PRODUCTION_DEPOSIT_CHAIN_IDS")
            || err.contains("not supported"),
        "TD-41: prod profile must reject chainId=1: {err}"
    );
}

#[test]
fn td_41_shellnet_profile_rejects_mainnet_chain_id() {
    let err = validate_chain_for_deployment_profile(
        ETHEREUM_MAINNET,
        DepositDeploymentProfile::ShellnetOrDev,
    )
    .unwrap_err();
    assert!(
        err.contains("not supported"),
        "TD-41: shellnet allowlist is six L2s + Sepolia only: {err}"
    );
}

#[test]
fn td_41_supported_list_is_six_l2_plus_sepolia_only() {
    assert_eq!(SUPPORTED_DEPOSIT_CHAIN_IDS.len(), 7);
    assert_eq!(PRODUCTION_DEPOSIT_CHAIN_IDS.len(), 6);
    for id in SUPPORTED_DEPOSIT_CHAIN_IDS {
        assert_ne!(*id, ETHEREUM_MAINNET);
    }
}
