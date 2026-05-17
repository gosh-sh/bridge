// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/AxiomBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeEventVerifier.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for the production bridge contract.
 * @dev Deploys an oracle + the bridge. Deposit/withdraw legacy verifier wiring
 *      was retired in Phase 4.3 (Decision Log 2026-05-17); ETH→AN deposit-event
 *      proofs are now consumed on the AN side natively (future `VERHALO2SHPLONK`
 *      TVM opcode). Future cross-chain withdrawals will land with a burn-proof
 *      circuit + state-anchored verification (post-Phase 7).
 *
 *      The AN→ETH `verifyBlock` triple (Primary / Fallback / LayerHashes) is
 *      intentionally left disabled here. A follow-up deployment step (Phase 5)
 *      wires the verifiers in once the partner produces a genesis BK-set
 *      Poseidon commitment on the target network.
 *
 * Oracle mode:
 *   - Set USE_AXIOM_ORACLE=true to deploy with AxiomBlockHeaderOracle (production)
 *   - Default (false) deploys MockBlockHeaderOracle (testing only)
 *
 * Axiom V2 Core addresses (from axiom-v2-contracts deployed.json):
 *   Mainnet: 0x69963768F8407dE501029680dE46945F838Fc98B
 *   Sepolia: 0x69963768F8407dE501029680dE46945F838Fc98B (mock, same address via CREATE3)
 */
contract DeployRealBridge is Script {
    // Axiom V2 Core addresses
    address constant AXIOM_V2_CORE_MAINNET = 0x69963768F8407dE501029680dE46945F838Fc98B;
    address constant AXIOM_V2_CORE_SEPOLIA = 0x69963768F8407dE501029680dE46945F838Fc98B;

    // AAVE V3 Ethereum mainnet addresses (https://github.com/bgd-labs/aave-address-book)
    address constant AAVE_V3_POOL_MAINNET = 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2;
    address constant AAVE_V3_WETH_GATEWAY_MAINNET = 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C;
    address constant AAVE_V3_aWETH_MAINNET = 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        bool useAxiomOracle = vm.envOr("USE_AXIOM_ORACLE", false);
        bool useAave = vm.envOr("USE_AAVE", false);

        vm.startBroadcast(deployerPrivateKey);

        // Step 1: Deploy block header oracle (kept for the future burn-proof flow).
        address oracleAddr;
        string memory oracleType;

        if (useAxiomOracle) {
            address axiomV2Core;
            if (block.chainid == 1) {
                axiomV2Core = AXIOM_V2_CORE_MAINNET;
            } else if (block.chainid == 11155111) {
                axiomV2Core = AXIOM_V2_CORE_SEPOLIA;
            } else {
                revert(
                    "Axiom oracle not supported on this chain. Use USE_AXIOM_ORACLE=false for local testing."
                );
            }
            require(axiomV2Core != address(0), "Axiom V2 Core address not set");

            console.log("Deploying AxiomBlockHeaderOracle...");
            console.log("  AxiomV2Core:", axiomV2Core);
            AxiomBlockHeaderOracle axiomOracle = new AxiomBlockHeaderOracle(axiomV2Core);
            oracleAddr = address(axiomOracle);
            oracleType = "AxiomBlockHeaderOracle";
        } else {
            console.log("Deploying MockBlockHeaderOracle (TESTING ONLY)...");
            console.log(
                "  WARNING: Mock oracle is NOT trustless. Use USE_AXIOM_ORACLE=true for production."
            );
            MockBlockHeaderOracle mockOracle = new MockBlockHeaderOracle();
            oracleAddr = address(mockOracle);
            oracleType = "MockBlockHeaderOracle";
        }
        console.log(string(abi.encodePacked(oracleType, " deployed at:")), oracleAddr);

        // Step 2: Pick AAVE addresses (only supported on mainnet, optional).
        address aavePool;
        address wethGateway;
        address aWETH;
        if (useAave) {
            require(block.chainid == 1, "AAVE wiring only supported on mainnet (chainid=1)");
            aavePool = AAVE_V3_POOL_MAINNET;
            wethGateway = AAVE_V3_WETH_GATEWAY_MAINNET;
            aWETH = AAVE_V3_aWETH_MAINNET;
            console.log("AAVE integration: ENABLED");
            console.log("  Pool:", aavePool);
            console.log("  WETH Gateway:", wethGateway);
            console.log("  aWETH:", aWETH);
        } else {
            console.log("AAVE integration: DISABLED (set USE_AAVE=true to enable on mainnet)");
        }

        // Step 3: Deploy the bridge contract with verifyBlock + verifyEvent
        //         initially disabled. Operators wire the verifiers in a
        //         follow-up deploy once VKs are finalised (see
        //         `docs/circuit_4_open_questions.md` for the Circuit 4
        //         Phase A/B split).
        AckiNackiBridge.VerifyBlockConfig memory vbDisabled = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0
        });
        AckiNackiBridge.BridgeEventConfig memory beDisabled = AckiNackiBridge.BridgeEventConfig({
            bridgeEventVerifier: IBridgeEventVerifier(address(0)), dappFr: 0, accFr: 0
        });
        console.log("Deploying AckiNackiBridge...");
        AckiNackiBridge bridge =
            new AckiNackiBridge(oracleAddr, aavePool, wethGateway, aWETH, vbDisabled, beDisabled);
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", address(bridge));

        if (!useAxiomOracle) {
            console.log(
                "\n  ** WARNING: Using MockBlockHeaderOracle -- NOT suitable for production **"
            );
            console.log(
                "  ** Re-deploy with USE_AXIOM_ORACLE=true for trustless block hash verification **"
            );
        }

        console.log("\nTo make a deposit:");
        console.log(
            "cast send",
            address(bridge),
            "\"deposit()\" --value 0.001ether --rpc-url $SEPOLIA_RPC_URL --private-key $PRIVATE_KEY"
        );
        console.log("\nSave these addresses for testing!");

        string memory deploymentJson = string(
            abi.encodePacked(
                "{\n",
                '  "oracle": "',
                vm.toString(oracleAddr),
                '",\n',
                '  "oracle_type": "',
                oracleType,
                '",\n',
                '  "bridge": "',
                vm.toString(address(bridge)),
                '"\n',
                "}"
            )
        );

        vm.writeFile("deployment_real.json", deploymentJson);
        console.log("\nDeployment info saved to: deployment_real.json");
    }
}
