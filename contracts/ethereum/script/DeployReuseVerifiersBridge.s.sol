// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

/// @title DeployReuseVerifiersBridge
/// @notice Deploy a fresh AckiNackiBridge that REUSES already-deployed Sepolia
///         verifier contracts (Primary/Fallback/LayerHashes/BridgeWithdrawal),
///         since those are circuit-VK-bound and segment-agnostic. Only the
///         genesis anchor + Circuit-4 identity/token config change per E2E run.
///         Used for the full AN→ETH new-block E2E where we need a bridge with an
///         empty nullifier map genesis'd to a fresh chain segment.
contract DeployReuseVerifiersBridge is Script {
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;

    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");

        AckiNackiBridge.VerifyBlockConfig memory vb = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(vm.envAddress("PRIMARY_VERIFIER")),
            fallbackVerifier: IFallbackVerifier(vm.envAddress("FALLBACK_VERIFIER")),
            layerHashesVerifier: ILayerHashesMovementVerifier(vm.envAddress("LAYER_HASHES_VERIFIER")),
            genesisBkSetCommitment: vm.envUint("GENESIS_BK_SET_COMMITMENT"),
            genesisPrevMaxLevelLayerHash: vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH")
        });

        AckiNackiBridge.BridgeWithdrawConfig memory bw = AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(vm.envAddress("WITHDRAWAL_VERIFIER")),
            dappFr: vm.envUint("WITHDRAW_DAPP_FR"),
            accFr: vm.envUint("WITHDRAW_ACC_FR"),
            altDstChainId: vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(1)),
            altDstHostChainId: vm.envOr("WITHDRAW_ALT_DST_HOST_CHAIN_ID", uint256(11_155_111)),
            altTokenId: vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(3))
        });

        bool startPaused = vm.envOr("START_PAUSED", false);

        vm.startBroadcast(pk);
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        AckiNackiBridge bridge =
            new AckiNackiBridge(address(oracle), USDC_SEPOLIA, address(0), address(0), vb, bw);
        if (startPaused) {
            bridge.pause();
        }
        vm.stopBroadcast();

        console.log("AckiNackiBridge (reuse-verifiers):", address(bridge));
        console.log("oracle:", address(oracle));
        console.log("primaryVerifier:", address(vb.primaryVerifier));
        console.log("fallbackVerifier:", address(vb.fallbackVerifier));
        console.log("layerHashesVerifier:", address(vb.layerHashesVerifier));
        console.log("bridgeWithdrawalVerifier:", address(bw.bridgeWithdrawalVerifier));
        console.log("startPaused:", startPaused);
    }
}
