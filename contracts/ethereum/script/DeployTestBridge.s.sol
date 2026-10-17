// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/testnet/TestnetAavePool.sol";

/**
 * @title DeployTestBridge
 * @notice Deployment script for local/testnet smoke-testing of the bridge.
 * @dev Deploys an oracle + bridge with `verifyBlock` disabled. Set `USDC_ADDRESS`
 *      env var on Sepolia (defaults to Aave-faucet test USDC). `USE_AAVE=true`
 *      wires the Aave V3 Sepolia pool and aUSDC; the pool lists only the faucet
 *      USDC, so it is refused together with another `USDC_ADDRESS`.
 *      `TESTNET_AAVE_POOL=true` (with `USE_AAVE=true`) deploys a
 *      `TestnetAavePool` over the faucet USDC instead, for when the real
 *      reserve is over its supply cap.
 */
contract DeployTestBridge is Script {
    // Aave V3 Sepolia USDC underlying (USDC-TestnetMintableERC20 from the
    // Aave faucet). Override via the `USDC_ADDRESS` env var.
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;
    address constant AAVE_V3_POOL_SEPOLIA = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant AAVE_V3_aUSDC_SEPOLIA = 0x16dA4541aD1807f4443d92D26044C1147406EB80;
    address constant AAVE_FAUCET_SEPOLIA = 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        address usdc = vm.envOr("USDC_ADDRESS", USDC_SEPOLIA);
        bool useAave = vm.envOr("USE_AAVE", false);
        require(!useAave || usdc == USDC_SEPOLIA, "USE_AAVE needs the Aave faucet USDC");

        bool testnetPool = vm.envOr("TESTNET_AAVE_POOL", false);
        require(!testnetPool || useAave, "TESTNET_AAVE_POOL needs USE_AAVE");

        vm.startBroadcast(deployerPrivateKey);

        address aavePool;
        address aUsdc;
        if (testnetPool) {
            TestnetAavePool pool = new TestnetAavePool(usdc, AAVE_FAUCET_SEPOLIA);
            aavePool = address(pool);
            aUsdc = address(pool.aToken());
            console.log("TestnetAavePool deployed at:", aavePool);
            console.log("TestnetAToken:", aUsdc);
        } else if (useAave) {
            aavePool = AAVE_V3_POOL_SEPOLIA;
            aUsdc = AAVE_V3_aUSDC_SEPOLIA;
        }

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
            altTokenId: 0
        });
        AckiNackiBridge bridge =
            new AckiNackiBridge(address(oracle), usdc, aavePool, aUsdc, vbDisabled, bwDisabled);
        console.log("AckiNackiBridge deployed at:", address(bridge));
        console.log("USDC:", usdc);
        console.log("AAVE enabled:", useAave);

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Bridge:", address(bridge));
        console.log("\nTo make a test deposit (fund USDC from Aave faucet first):");
        console.log("  mint USDC via faucet 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D");
        console.log("  then approve + deposit(uint256 amount) on the bridge");
    }
}
