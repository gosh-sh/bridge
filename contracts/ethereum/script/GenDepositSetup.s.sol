// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../test/mocks/MockERC20.sol";

/**
 * @title GenDepositSetup
 * @notice Local-only helper: deploys a mock USDC + oracle + bridge (AAVE /
 *         verifyBlock / withdraw all disabled), then mints + approves USDC for
 *         the deployer so a `cast`-driven loop can emit 10 distinct `Deposit`
 *         events for real proof generation. Not for any live network.
 */
contract GenDepositSetup is Script {
    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");
        address deployer = vm.addr(pk);

        vm.startBroadcast(pk);

        MockERC20 usdc = new MockERC20("USD Coin (test)", "USDC", 6);
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();

        AckiNackiBridge.VerifyBlockConfig memory vbDisabled = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0
        });
        AckiNackiBridge.BridgeWithdrawConfig memory bwDisabled = AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0)),
            dappFr: 0,
            accFr: 0,
            altDstChainId: 0,
            altDstHostChainId: 0,
            altTokenId: 0
        });

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle), address(usdc), address(0), address(0), vbDisabled, bwDisabled
        );

        // Fund the deployer generously and pre-approve the bridge for max.
        usdc.mint(deployer, 1_000_000 * 1e6);
        usdc.approve(address(bridge), type(uint256).max);

        vm.stopBroadcast();

        console.log("USDC:", address(usdc));
        console.log("ORACLE:", address(oracle));
        console.log("BRIDGE:", address(bridge));
    }
}
