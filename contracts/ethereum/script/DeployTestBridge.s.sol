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
 * @dev Deploys an oracle + bridge with `verifyBlock` disabled. Set `USDC_ADDRESS`
 *      env var on Sepolia (defaults to Aave-faucet test USDC).
 */
contract DeployTestBridge is Script {
    // Aave V3 Sepolia USDC underlying (USDC-TestnetMintableERC20 from the
    // Aave faucet). Override via the `USDC_ADDRESS` env var.
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        address usdc = vm.envOr("USDC_ADDRESS", USDC_SEPOLIA);

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
            address(oracle), usdc, address(0), address(0), vbDisabled, bwDisabled
        );
        console.log("AckiNackiBridge deployed at:", address(bridge));
        console.log("USDC:", usdc);

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Bridge:", address(bridge));
        console.log("\nTo make a test deposit (fund USDC from Aave faucet first):");
        console.log("  mint USDC via faucet 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D");
        console.log("  then approve + deposit(uint256 amount) on the bridge");
    }
}
