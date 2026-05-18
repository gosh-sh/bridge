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
import "../src/PrimaryGroth16VerifierGenerated.sol";
import "../src/FallbackGroth16VerifierGenerated.sol";
import "../src/LayerHashesGroth16VerifierGenerated.sol";
import "../src/PrimaryVerifier.sol";
import "../src/FallbackVerifier.sol";
import "../src/LayerHashesMovementVerifier.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for the production bridge contract.
 * @dev Deploys an oracle + (optionally) the verifyBlock verifier triple +
 *      the bridge. Legacy deposit/withdraw verifier wiring was retired in
 *      Phase 4.3 (Decision Log 2026-05-17); ETH→AN deposit-event proofs are
 *      now consumed on the AN side natively (future `VERHALO2SHPLONK` TVM
 *      opcode). Future cross-chain withdrawals will land with a burn-proof
 *      circuit + state-anchored verification (post-Phase 7).
 *
 *      The bridge constructor is the **only** entry point that can set the
 *      verifier addresses (no admin setter, by design). So the verifyBlock
 *      triple has to be wired at deploy time or the bridge is permanently
 *      `VerifyBlockDisabled` and has to be re-deployed.
 *
 * Oracle mode:
 *   - Set USE_AXIOM_ORACLE=true to deploy with AxiomBlockHeaderOracle (production)
 *   - Default (false) deploys MockBlockHeaderOracle (testing only)
 *
 * verifyBlock wiring mode:
 *   - Set WIRE_VERIFY_BLOCK=true to deploy the Primary/Fallback/LayerHashes
 *     verifier triple (Groth16-generated + adapter contracts) and wire them
 *     into the bridge constructor. Requires GENESIS_BK_SET_COMMITMENT
 *     (uint256, Poseidon commitment of the genesis BK set) and
 *     GENESIS_PREV_MAX_LEVEL_LAYER_HASH (uint256, chain anchor) to be set.
 *   - Default (false) deploys with all three verifier slots = address(0).
 *     `verifyBlock` then reverts with `VerifyBlockDisabled` and the bridge
 *     must be re-deployed to enable AN→ETH state transitions.
 *
 * AAVE mode:
 *   - Set USE_AAVE=true to wire AAVE V3 (mainnet only).
 *
 * Circuit 4 (verifyEvent) wiring is *not* exposed here: there is no
 * gnark-generated `BridgeEventGroth16VerifierGenerated.sol` yet (Phase A
 * scaffolding only, Phase B blocked on the open questions in
 * `docs/circuit_4_open_questions.md`).
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
        bool wireVerifyBlock = vm.envOr("WIRE_VERIFY_BLOCK", false);

        // If wiring is requested, both anchors must be provided. We fetch
        // them up-front (envUint reverts on missing var) so a missing
        // anchor fails the deploy *before* any contract is broadcast.
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
        if (wireVerifyBlock) {
            genesisBkSetCommitment = vm.envUint("GENESIS_BK_SET_COMMITMENT");
            genesisPrevAnchor = vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH");
            require(
                genesisBkSetCommitment != 0,
                "GENESIS_BK_SET_COMMITMENT must be non-zero when WIRE_VERIFY_BLOCK=true"
            );
            // genesisPrevAnchor == 0 is legitimate (genesis chain head).
        }

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

        // Step 3: Optionally deploy the AN→ETH verifier triple (Primary /
        //         Fallback / LayerHashes). The constructor of `AckiNackiBridge`
        //         is the only place verifier addresses can be set — no
        //         post-deploy setter — so wiring must happen here.
        AckiNackiBridge.VerifyBlockConfig memory vb;
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        if (wireVerifyBlock) {
            console.log("Deploying Groth16 verifier triple...");

            PrimaryGroth16VerifierGenerated pG = new PrimaryGroth16VerifierGenerated();
            console.log("  PrimaryGroth16VerifierGenerated:", address(pG));
            PrimaryVerifier pv = new PrimaryVerifier(address(pG));
            primaryVerifierAddr = address(pv);
            console.log("  PrimaryVerifier (adapter):", primaryVerifierAddr);

            FallbackGroth16VerifierGenerated fG = new FallbackGroth16VerifierGenerated();
            console.log("  FallbackGroth16VerifierGenerated:", address(fG));
            FallbackVerifier fv = new FallbackVerifier(address(fG));
            fallbackVerifierAddr = address(fv);
            console.log("  FallbackVerifier (adapter):", fallbackVerifierAddr);

            LayerHashesGroth16VerifierGenerated lG = new LayerHashesGroth16VerifierGenerated();
            console.log("  LayerHashesGroth16VerifierGenerated:", address(lG));
            LayerHashesMovementVerifier lv = new LayerHashesMovementVerifier(address(lG));
            layerHashesVerifierAddr = address(lv);
            console.log("  LayerHashesMovementVerifier (adapter):", layerHashesVerifierAddr);

            vb = AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: IPrimaryVerifier(primaryVerifierAddr),
                fallbackVerifier: IFallbackVerifier(fallbackVerifierAddr),
                layerHashesVerifier: ILayerHashesMovementVerifier(layerHashesVerifierAddr),
                genesisBkSetCommitment: genesisBkSetCommitment,
                genesisPrevMaxLevelLayerHash: genesisPrevAnchor
            });
            console.log("verifyBlock anchors:");
            console.log("  genesis BK-set commitment:", genesisBkSetCommitment);
            console.log("  genesis prev-anchor      :", genesisPrevAnchor);
        } else {
            console.log("verifyBlock wiring: DISABLED (set WIRE_VERIFY_BLOCK=true to wire)");
            vb = AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: IPrimaryVerifier(address(0)),
                fallbackVerifier: IFallbackVerifier(address(0)),
                layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
                genesisBkSetCommitment: 0,
                genesisPrevMaxLevelLayerHash: 0
            });
        }

        // Step 4: Circuit 4 (verifyEvent) is always disabled in this script —
        //         no gnark-generated `BridgeEventGroth16VerifierGenerated`
        //         exists yet (Phase A scaffolding only; Phase B blocked on
        //         `docs/circuit_4_open_questions.md`).
        AckiNackiBridge.BridgeEventConfig memory beDisabled = AckiNackiBridge.BridgeEventConfig({
            bridgeEventVerifier: IBridgeEventVerifier(address(0)), dappFr: 0, accFr: 0
        });

        // Step 5: Deploy the bridge wired against the configs above.
        console.log("Deploying AckiNackiBridge...");
        AckiNackiBridge bridge =
            new AckiNackiBridge(oracleAddr, aavePool, wethGateway, aWETH, vb, beDisabled);
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", address(bridge));
        if (wireVerifyBlock) {
            console.log("PrimaryVerifier (adapter):", primaryVerifierAddr);
            console.log("FallbackVerifier (adapter):", fallbackVerifierAddr);
            console.log("LayerHashesMovementVerifier (adapter):", layerHashesVerifierAddr);
        }

        if (!useAxiomOracle) {
            console.log(
                "\n  ** WARNING: Using MockBlockHeaderOracle -- NOT suitable for production **"
            );
            console.log(
                "  ** Re-deploy with USE_AXIOM_ORACLE=true for trustless block hash verification **"
            );
        }
        if (!wireVerifyBlock) {
            console.log("\n  ** WARNING: verifyBlock is DISABLED on this bridge instance. **");
            console.log("  ** AN->ETH state transitions will revert with VerifyBlockDisabled. **");
            console.log(
                "  ** The bridge constructor is the only entry point that wires verifiers; **"
            );
            console.log(
                "  ** re-deploy with WIRE_VERIFY_BLOCK=true once the genesis BK-set commitment is known. **"
            );
        }

        console.log("\nSave these addresses for testing!");

        string memory verifierJson = "";
        if (wireVerifyBlock) {
            verifierJson = string(
                abi.encodePacked(
                    '  "primary_verifier": "',
                    vm.toString(primaryVerifierAddr),
                    '",\n',
                    '  "fallback_verifier": "',
                    vm.toString(fallbackVerifierAddr),
                    '",\n',
                    '  "layer_hashes_verifier": "',
                    vm.toString(layerHashesVerifierAddr),
                    '",\n',
                    '  "genesis_bk_set_commitment": "',
                    vm.toString(genesisBkSetCommitment),
                    '",\n',
                    '  "genesis_prev_max_level_layer_hash": "',
                    vm.toString(genesisPrevAnchor),
                    '",\n'
                )
            );
        }

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
                '",\n',
                '  "verify_block_wired": ',
                wireVerifyBlock ? "true" : "false",
                ",\n",
                verifierJson,
                '  "aave_enabled": ',
                useAave ? "true" : "false",
                "\n}"
            )
        );

        vm.writeFile("deployment_real.json", deploymentJson);
        console.log("\nDeployment info saved to: deployment_real.json");
    }
}
