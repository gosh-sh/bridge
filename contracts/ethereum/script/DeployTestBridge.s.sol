// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

/**
 * @title DeployTestBridge
 * @notice Deployment script for local/testnet smoke-testing of the bridge.
 * @dev Deploys an oracle + bridge with `verifyBlock` disabled. Set `USDT_ADDRESS`
 *      env var on Sepolia (defaults to Aave-faucet test USDT).
 */
contract DeployTestBridge is Script {
    address constant USDT_SEPOLIA = 0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        address usdt = vm.envOr("USDT_ADDRESS", USDT_SEPOLIA);

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
        AckiNackiBridge.BridgeWithdrawConfig memory bwDisabled = AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0)), dappFr: 0, accFr: 0
        });
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle), usdt, address(0), address(0), vbDisabled, bwDisabled
        );
        console.log("AckiNackiBridge deployed at:", address(bridge));
        console.log("USDT:", usdt);

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Bridge:", address(bridge));
        console.log("\nTo make a test deposit (fund USDT from Aave faucet first):");
        console.log("  mint USDT via faucet 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D");
        console.log("  then approve + deposit(uint256 amount) on the bridge");
    }
}
