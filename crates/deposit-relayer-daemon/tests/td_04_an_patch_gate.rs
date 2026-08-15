//! TD-04 — AN deposit security gate (bridge repo synced contracts + mocks).
//!
//! Supports audit overlay `USDCBridge.sol` (legacy patch) and upstream
//! `eccUSDCBridge.sol` (`acki-nacki@contracts/bridge`).

use std::path::PathBuf;

use deposit_relayer_daemon::types::NUM_PUBLIC_INPUTS;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .canonicalize()
        .expect("repo root")
}

fn read_repo_file(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| {
        panic!("TD-04: failed to read {rel}: {e}")
    })
}

fn exchange_dir() -> PathBuf {
    repo_root().join("audit/spec/an-contracts/exchange")
}

fn audit_bridge_source() -> String {
    let dir = exchange_dir();
    if let Ok(s) = std::fs::read_to_string(dir.join("eccUSDCBridge.sol")) {
        return s;
    }
    read_repo_file("audit/spec/an-contracts/exchange/USDCBridge.sol")
}

fn is_ecc_usdc_bridge() -> bool {
    exchange_dir().join("eccUSDCBridge.sol").is_file()
}

fn audit_deposit_voucher_source() -> String {
    read_repo_file("audit/spec/an-contracts/exchange/DepositVoucher.sol")
}

fn patch_series_source() -> String {
    read_repo_file("USDCBridge_12pi_chainid_allowlist.patch")
}

#[test]
fn td_04_bridge_prover_twelve_public_inputs_aligned() {
    assert_eq!(NUM_PUBLIC_INPUTS, 12, "bridge relayer/prover expect 12-PI layout");
}

#[test]
fn td_04_patch_series_lists_required_security_items() {
    let patch = patch_series_source();
    assert!(patch.contains("_expectedBridgeFr"), "patch: chain allowlist");
    assert!(patch.contains("_expectedAnDappId"), "patch: dappId gate");
    assert!(patch.contains("_acceptedBlockHash"), "patch: anchor gate");
    assert!(patch.contains("srcChainId"), "patch: DEP-N-5 identity");
    assert!(patch.contains("_mintCapByChain"), "patch: per-chain mint cap");
    assert!(patch.contains("attestBlockHash"), "patch: M-of-N attester path");
    assert!(patch.contains("ERR_UNKNOWN_SOURCE"), "patch: allowlist error 222");
    assert!(patch.contains("ERR_WRONG_DAPP"), "patch: dapp error 223");
    assert!(patch.contains("ERR_UNKNOWN_BLOCK"), "patch: anchor error 224");
}

#[test]
fn td_04_audit_bridge_has_twelve_pi_parser() {
    let src = audit_bridge_source();
    assert!(
        src.contains("for (uint k = 0; k < 9; k++"),
        "bridge: 12-PI parser reads 9 Fr"
    );
    assert!(
        src.contains("12 × 32-byte LE Fr"),
        "bridge documents 12-PI publicInputs column"
    );
    assert!(
        !src.contains("for (uint k = 0; k < 8; k++"),
        "bridge must not retain legacy 8-Fr parser loop"
    );
}

#[test]
fn td_04_audit_bridge_allowlist_model() {
    let src = audit_bridge_source();
    if is_ecc_usdc_bridge() {
        assert!(src.contains("_trustedL1Bridge"), "eccUSDCBridge: SET allowlist");
        assert!(src.contains("setTrustedL1Bridge"), "eccUSDCBridge: owner setter");
        assert!(src.contains("ERR_UNSUPPORTED_SRC_CHAIN"), "eccUSDCBridge: error 222");
        assert!(
            src.contains("f.dappId       = 0"),
            "eccUSDCBridge: dappId pinned to 0"
        );
    } else {
        assert!(src.contains("_expectedBridgeFr"), "overlay: chain allowlist");
        assert!(src.contains("_expectedAnDappId"), "overlay: dappId gate");
        assert!(src.contains("_acceptedBlockHash"), "overlay: anchor gate");
        assert!(src.contains("_mintCapByChain"), "overlay: per-chain mint cap");
        assert!(src.contains("attestBlockHash"), "overlay: M-of-N attesters");
        assert!(src.contains("_depositIdentity"), "overlay: DEP-N-5 identity helper");
        assert!(src.contains("ERR_UNKNOWN_SOURCE"), "overlay: error 222");
        assert!(src.contains("ERR_WRONG_DAPP"), "overlay: error 223");
        assert!(src.contains("ERR_UNKNOWN_BLOCK"), "overlay: error 224");
    }
}

#[test]
fn td_04_audit_voucher_identity_includes_chain_binding() {
    let bridge = audit_bridge_source();
    let voucher = audit_deposit_voucher_source();
    if is_ecc_usdc_bridge() {
        assert!(
            bridge.contains("abi.encode(f.depositId, f.contractAddr, f.dappId, f.chainId)"),
            "eccUSDCBridge: replay hash includes chainId"
        );
        assert!(voucher.contains("chainId"), "DepositVoucher: chainId constructor arg");
        assert!(
            voucher.contains("abi.encode(depositId, contractAddr, dappId, chainId)"),
            "DepositVoucher: hash includes chainId"
        );
    } else {
        assert!(
            bridge.contains("abi.encode(srcChainId, depositId, contractAddr, dappId)"),
            "overlay: identity hash includes srcChainId"
        );
        assert!(voucher.contains("srcChainId"), "overlay: DepositVoucher srcChainId");
        assert!(
            voucher.contains("abi.encode(srcChainId, depositId, contractAddr, dappId)"),
            "overlay: voucher hash includes srcChainId"
        );
    }
}

#[test]
fn td_04_mock_coverage_td02_td03_td15_registry() {
    let mocks = [
        ("12-PI layout / legacy drift", "td_02_pi_layout_drift.rs"),
        ("anchor gate ERR_UNKNOWN_BLOCK 224", "td_03_forged_block_anchor.rs"),
        ("DEP-N-5 srcChainId identity", "td_15_deposit_id_collision.rs"),
        ("dappId injection ERR_WRONG_DAPP 223", "td_05_dapp_id_injection.rs"),
    ];
    for (behaviour, file) in mocks {
        let path = repo_root().join("crates/deposit-relayer-daemon/tests").join(file);
        assert!(path.is_file(), "TD-04 mock PoC missing for {behaviour}: {file}");
    }
}

#[test]
fn td_04_voucher_abi_check_script_path_exists() {
    let script = repo_root().join("scripts/check_voucher_abi_consistency.py");
    assert!(script.is_file(), "voucher ABI gate script must exist");
    let exchange = exchange_dir();
    assert!(
        exchange.join("eccUSDCBridge.sol").is_file() || exchange.join("USDCBridge.sol").is_file(),
        "audit exchange must have eccUSDCBridge.sol or USDCBridge.sol"
    );
    assert!(
        exchange.join("DepositVoucher.sol").is_file(),
        "audit exchange DepositVoucher.sol for --source-only gate"
    );
}

#[test]
fn td_04_deploy_readiness_markers_present() {
    let src = audit_bridge_source();
    if is_ecc_usdc_bridge() {
        let markers = ["_trustedL1Bridge", "setTrustedL1Bridge"];
        let missing: Vec<&str> = markers
            .iter()
            .filter(|m| !src.contains(*m))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "TD-04: eccUSDCBridge missing deploy-readiness markers: {missing:?}"
        );
    } else {
        let patch_markers = [
            "_expectedBridgeFr",
            "_acceptedBlockHash",
            "_depositIdentity",
            "setExpectedBridge",
            "setExpectedAnDappId",
            "setMintCap",
            "attestBlockHash",
        ];
        let missing: Vec<&str> = patch_markers
            .iter()
            .filter(|m| !src.contains(*m))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "TD-04: overlay missing deploy-readiness markers: {missing:?}"
        );
    }
}
