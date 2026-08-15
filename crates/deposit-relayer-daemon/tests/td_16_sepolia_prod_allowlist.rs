//! TD-16 — Sepolia must not appear on production deposit deployment profiles.

use deposit_relayer_daemon::{
    CHAIN_ID_BASE, CHAIN_ID_SEPOLIA, DepositDeploymentProfile,
    is_production_deposit_chain, is_testnet_only_deposit_chain,
    parse_deployment_profile, production_and_testnet_only_disjoint,
    validate_chain_for_deployment_profile,
};

#[test]
fn td_16_prod_and_testnet_only_sets_disjoint() {
    assert!(production_and_testnet_only_disjoint());
    assert!(!is_production_deposit_chain(CHAIN_ID_SEPOLIA));
    assert!(is_testnet_only_deposit_chain(CHAIN_ID_SEPOLIA));
}

#[test]
fn td_16_prod_profile_rejects_sepolia_chain_id() {
    let err = validate_chain_for_deployment_profile(
        CHAIN_ID_SEPOLIA,
        DepositDeploymentProfile::Production,
    )
    .unwrap_err();
    assert!(err.contains("testnet-only"));
}

#[test]
fn td_16_shellnet_profile_accepts_sepolia() {
    validate_chain_for_deployment_profile(
        CHAIN_ID_SEPOLIA,
        DepositDeploymentProfile::ShellnetOrDev,
    )
    .unwrap();
}

#[test]
fn td_16_prod_profile_accepts_base_l2() {
    validate_chain_for_deployment_profile(
        CHAIN_ID_BASE,
        DepositDeploymentProfile::Production,
    )
    .unwrap();
}

#[test]
fn td_16_parse_profile_prod_and_shellnet() {
    assert_eq!(
        parse_deployment_profile("prod"),
        Some(DepositDeploymentProfile::Production)
    );
    assert_eq!(
        parse_deployment_profile("shellnet"),
        Some(DepositDeploymentProfile::ShellnetOrDev)
    );
}
