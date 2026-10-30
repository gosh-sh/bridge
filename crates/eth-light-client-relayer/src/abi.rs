//! The Acki Nacki contract ABIs, built into the binary (bridge-audit#132):
//! no `*_ABI_PATH` flag, no ABI file read at runtime. Each is the compiled
//! ABI of the contract the relayer talks to, the same copy the CLI builds
//! in; `scripts/check_lc_relayer_abis.py` checks that every function the
//! relayer calls is in it.

/// `EthBeaconLightClient`: `submitUpdate`, `submitRotate`, `submitAncestry`,
/// `rePushAnchor`, the owner calls and the getters.
pub const LIGHT_CLIENT_ABI_JSON: &str =
    include_str!("../../../contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.abi.json");

/// `eccUSDCBridge`: the flip (`getLightClient`, `isOwnerAnchorsEnabled`,
/// `disableOwnerAnchorsOn`) and the anchors (`getL2Verifier`,
/// `isAcceptedBlockHash`). The full ABI, shared with the CLI and kept in
/// sync with the contract by `scripts/check_bridge_abi_in_sync.sh`.
pub const BRIDGE_ABI_JSON: &str = include_str!("../../ackinacki-bridge/abi/USDCBridge.abi.json");

/// `ArbitrumBlockVerifier`: `proveBlock` and `getConfig`.
pub const ARBITRUM_VERIFIER_ABI_JSON: &str =
    include_str!("../../../contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.abi.json");

#[cfg(feature = "live-submit")]
pub fn light_client_abi() -> tvm_client::abi::Abi {
    tvm_client::abi::Abi::Json(LIGHT_CLIENT_ABI_JSON.to_string())
}

#[cfg(feature = "live-submit")]
pub fn bridge_abi() -> tvm_client::abi::Abi {
    tvm_client::abi::Abi::Json(BRIDGE_ABI_JSON.to_string())
}

#[cfg(feature = "live-submit")]
pub fn arbitrum_verifier_abi() -> tvm_client::abi::Abi {
    tvm_client::abi::Abi::Json(ARBITRUM_VERIFIER_ABI_JSON.to_string())
}

/// The function names of one of the ABIs above.
pub fn function_names(abi_json: &str) -> Vec<String> {
    let abi: serde_json::Value = serde_json::from_str(abi_json).expect("built-in ABI is JSON");
    abi["functions"]
        .as_array()
        .map(|fns| {
            fns.iter()
                .filter_map(|f| f["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_function_the_relayer_calls_is_in_its_built_in_abi() {
        let lc = function_names(LIGHT_CLIENT_ABI_JSON);
        for f in [
            "submitUpdate",
            "submitRotate",
            "submitAncestry",
            "rePushAnchor",
            "disableOwnerRotation",
            "setCommitteeCommitment",
            "setGloasForkSlot",
            "getGloasForkSlot",
            "getCommitteeState",
            "getConfig",
        ] {
            assert!(lc.iter().any(|n| n == f), "light client ABI lacks {f}");
        }
        let bridge = function_names(BRIDGE_ABI_JSON);
        for f in [
            "getLightClient",
            "isOwnerAnchorsEnabled",
            "disableOwnerAnchorsOn",
            "getL2Verifier",
            "isAcceptedBlockHash",
        ] {
            assert!(bridge.iter().any(|n| n == f), "bridge ABI lacks {f}");
        }
        let verifier = function_names(ARBITRUM_VERIFIER_ABI_JSON);
        for f in ["proveBlock", "getConfig"] {
            assert!(verifier.iter().any(|n| n == f), "verifier ABI lacks {f}");
        }
    }
}
