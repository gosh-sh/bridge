// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../test/mocks/MockAave.sol";

/**
 * @title DeployTestBridge
 * @notice Deployment script for local anvil (31337) smoke-testing of the bridge.
 * @dev Deploys an oracle, a mock AAVE market for `USDC_ADDRESS` (required)
 *      and a bridge with `verifyBlock` disabled. Any other chain reverts:
 *      public chains get `DeployRealBridge` or `DeployShellnetE2EBridge`.
 */
contract DeployTestBridge is Script {
    function run() external {
        require(block.chainid == 31337, "DeployTestBridge: local anvil only");
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        address usdc = vm.envAddress("USDC_ADDRESS");

        vm.startBroadcast(deployerPrivateKey);

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle deployed at:", address(oracle));

        AckiNackiBridge.VerifyBlockConfig memory vbDisabled = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0,
            genesisLastSeenBlockSeqNo: 0,
            bundleStride: 0,
            genesisLastBkSetUpdateSeqNo: 0
        });
        AckiNackiBridge.BridgeWithdrawConfig memory bwDisabled = AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0)),
            dappFr: 0,
            accFr: 0,
            altDstChainId: 0,
            altDstHostChainId: 0,
            altTokenId: 0,
            predecessor: IPredecessorBridge(address(0))
        });
        MockAUSDC aUsdc = new MockAUSDC();
        MockAavePool pool = new MockAavePool(usdc, address(aUsdc));
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle), usdc, address(pool), address(aUsdc), vbDisabled, bwDisabled
        );
        console.log("AckiNackiBridge deployed at:", address(bridge));
        console.log("USDC:", usdc);

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Network: anvil");
        console.log("Bridge:", address(bridge));
        console.log("Mock AAVE pool:", address(pool));
        console.log("\nTo make a test deposit: approve + deposit(amount, anAccount) on the bridge");
    }
}
