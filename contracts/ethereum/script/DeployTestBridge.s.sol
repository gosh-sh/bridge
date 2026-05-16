// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";

/**
 * @title DeployTestBridge
 * @notice Deployment script for local/testnet smoke-testing of the bridge.
 * @dev Deposit/withdraw legacy verifier wiring was retired in Phase 4.3
 *      (Decision Log 2026-05-17). This script now only deploys an oracle +
 *      the bridge with `verifyBlock` disabled — enough to exercise `deposit()`
 *      end-to-end on a testnet.
 */
contract DeployTestBridge is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle deployed at:", address(oracle));

        AckiNackiBridge.VerifyBlockConfig memory vbDisabled = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0
        });
        AckiNackiBridge bridge =
            new AckiNackiBridge(address(oracle), address(0), address(0), address(0), vbDisabled);
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Bridge:", address(bridge));
        console.log("\nTo make a test deposit:");
        console.log(
            "cast send",
            address(bridge),
            "\"deposit()\" --value 0.1ether --rpc-url $SEPOLIA_RPC_URL --private-key $PRIVATE_KEY"
        );
    }
}
