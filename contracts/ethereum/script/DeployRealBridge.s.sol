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
import "../src/IBridgeWithdrawalVerifier.sol";
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
    // Bundle the JSON-writer's many fields into a single memory struct.
    // Without this packing the helper has 10 plain parameters and trips
    // "Stack too deep" under the coverage profile (optimizer + viaIR are
    // both disabled there so each parameter consumes a separate stack
    // slot; see pipeline #5744).
    struct DeploymentJsonArgs {
        address oracleAddr;
        string oracleType;
        address bridgeAddr;
        bool useAave;
        bool wireVerifyBlock;
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
    }

    // Axiom V2 Core addresses
    address constant AXIOM_V2_CORE_MAINNET = 0x69963768F8407dE501029680dE46945F838Fc98B;
    address constant AXIOM_V2_CORE_SEPOLIA = 0x69963768F8407dE501029680dE46945F838Fc98B;

    // AAVE V3 Ethereum mainnet addresses (https://github.com/bgd-labs/aave-address-book)
    address constant AAVE_V3_POOL_MAINNET = 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2;
    address constant AAVE_V3_WETH_GATEWAY_MAINNET = 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C;
    address constant AAVE_V3_aWETH_MAINNET = 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8;

    function run() external {
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

        // Scope `deployerPrivateKey` to just the broadcast handshake so
        // it doesn't pin a stack slot through the rest of the function
        // (the coverage profile is *very* tight on stack depth — see the
        // `DeploymentJsonArgs` comment).
        {
            uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
            vm.startBroadcast(deployerPrivateKey);
        }

        // Step 1: Deploy block header oracle (kept for the future burn-proof flow).
        (address oracleAddr, string memory oracleType) = _deployOracle(useAxiomOracle);

        // Step 2 + 3 + 4 + 5 are bundled into a scope block so the AAVE
        // address locals, `vb`, `beDisabled`, and `bridge` fall out of
        // scope before the JSON assembly below — otherwise `run()` carries
        // 17+ live locals into the `DeploymentJsonArgs` literal and the
        // coverage profile (optimizer + viaIR off) hits "Stack too deep".
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        address bridgeAddr;
        {
            // Step 2: AAVE addresses.
            (address aavePool, address wethGateway, address aWETH) = _resolveAaveAddresses(useAave);

            // Step 3: optional AN→ETH verifier triple. Extracted into a
            //         helper so the six per-contract locals don't pile up
            //         here either.
            AckiNackiBridge.VerifyBlockConfig memory vb = _buildVerifyBlockConfig(
                wireVerifyBlock, genesisBkSetCommitment, genesisPrevAnchor
            );
            if (wireVerifyBlock) {
                primaryVerifierAddr = address(vb.primaryVerifier);
                fallbackVerifierAddr = address(vb.fallbackVerifier);
                layerHashesVerifierAddr = address(vb.layerHashesVerifier);
            }

            // Step 4 + 5: Circuit 4 (verifyEvent) + Circuit 4 v2 (withdrawByProof)
            //             both disabled here (no gnark-generated verifiers yet —
            //             Phase A attestation scaffolding only, Phase B payout
            //             pending partner's v2 circuit; see
            //             docs/an_partner_questions_circuit4_2026-05-17.md).
            //             Deploy the bridge wired against the configs above.
            console.log("Deploying AckiNackiBridge...");
            AckiNackiBridge bridge = new AckiNackiBridge(
                oracleAddr,
                aavePool,
                wethGateway,
                aWETH,
                vb,
                AckiNackiBridge.BridgeEventConfig({
                    bridgeEventVerifier: IBridgeEventVerifier(address(0)), dappFr: 0, accFr: 0
                }),
                AckiNackiBridge.BridgeWithdrawConfig({
                    bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0))
                })
            );
            bridgeAddr = address(bridge);
            console.log("AckiNackiBridge deployed at:", bridgeAddr);
        }

        vm.stopBroadcast();

        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", bridgeAddr);
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

        // JSON assembly extracted to a helper, with args packed into a
        // single memory struct to keep both `run()` and the helper itself
        // under the coverage profile's stack-depth limit. See the
        // `DeploymentJsonArgs` definition + comment above.
        _writeDeploymentJson(
            DeploymentJsonArgs({
                oracleAddr: oracleAddr,
                oracleType: oracleType,
                bridgeAddr: bridgeAddr,
                useAave: useAave,
                wireVerifyBlock: wireVerifyBlock,
                primaryVerifierAddr: primaryVerifierAddr,
                fallbackVerifierAddr: fallbackVerifierAddr,
                layerHashesVerifierAddr: layerHashesVerifierAddr,
                genesisBkSetCommitment: genesisBkSetCommitment,
                genesisPrevAnchor: genesisPrevAnchor
            })
        );
        console.log("\nDeployment info saved to: deployment_real.json");
    }

    /// Deploy the block header oracle. Extracted out of `run()` so the
    /// per-oracle local (`axiomOracle` or `mockOracle`) and `axiomV2Core`
    /// don't pile up in run()'s frame under the coverage profile.
    function _deployOracle(bool useAxiomOracle)
        internal
        returns (address oracleAddr, string memory oracleType)
    {
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
            oracleAddr = address(new AxiomBlockHeaderOracle(axiomV2Core));
            oracleType = "AxiomBlockHeaderOracle";
        } else {
            console.log("Deploying MockBlockHeaderOracle (TESTING ONLY)...");
            console.log(
                "  WARNING: Mock oracle is NOT trustless. Use USE_AXIOM_ORACLE=true for production."
            );
            oracleAddr = address(new MockBlockHeaderOracle());
            oracleType = "MockBlockHeaderOracle";
        }
        console.log(string(abi.encodePacked(oracleType, " deployed at:")), oracleAddr);
    }

    /// Resolve the AAVE V3 addresses to wire into the bridge constructor.
    /// All-zero unless `useAave == true` *and* we're on mainnet. Extracted
    /// out of run() to free three stack slots (the three addresses live
    /// only until the bridge constructor is called).
    function _resolveAaveAddresses(bool useAave)
        internal
        view
        returns (address aavePool, address wethGateway, address aWETH)
    {
        if (!useAave) {
            console.log("AAVE integration: DISABLED (set USE_AAVE=true to enable on mainnet)");
            return (address(0), address(0), address(0));
        }
        require(block.chainid == 1, "AAVE wiring only supported on mainnet (chainid=1)");
        aavePool = AAVE_V3_POOL_MAINNET;
        wethGateway = AAVE_V3_WETH_GATEWAY_MAINNET;
        aWETH = AAVE_V3_aWETH_MAINNET;
        console.log("AAVE integration: ENABLED");
        console.log("  Pool:", aavePool);
        console.log("  WETH Gateway:", wethGateway);
        console.log("  aWETH:", aWETH);
    }

    /// Build the `VerifyBlockConfig` struct, deploying the Primary /
    /// Fallback / LayerHashes Groth16 verifier triple + their adapter
    /// contracts when `wire == true`, or returning the all-zero "disabled"
    /// config otherwise. Extracted out of `run()` so the half-dozen per-
    /// contract locals stay in this frame rather than `run()`'s — the
    /// coverage profile (optimizer + viaIR off) cannot afford them in
    /// `run()`. Called from inside `vm.startBroadcast(...)` so the
    /// deployment calls are recorded as broadcast tx's.
    function _buildVerifyBlockConfig(
        bool wire,
        uint256 genesisBkSetCommitment,
        uint256 genesisPrevAnchor
    ) internal returns (AckiNackiBridge.VerifyBlockConfig memory vb) {
        if (!wire) {
            console.log("verifyBlock wiring: DISABLED (set WIRE_VERIFY_BLOCK=true to wire)");
            return AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: IPrimaryVerifier(address(0)),
                fallbackVerifier: IFallbackVerifier(address(0)),
                layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
                genesisBkSetCommitment: 0,
                genesisPrevMaxLevelLayerHash: 0
            });
        }

        console.log("Deploying Groth16 verifier triple...");

        PrimaryGroth16VerifierGenerated pG = new PrimaryGroth16VerifierGenerated();
        console.log("  PrimaryGroth16VerifierGenerated:", address(pG));
        PrimaryVerifier pv = new PrimaryVerifier(address(pG));
        console.log("  PrimaryVerifier (adapter):", address(pv));

        FallbackGroth16VerifierGenerated fG = new FallbackGroth16VerifierGenerated();
        console.log("  FallbackGroth16VerifierGenerated:", address(fG));
        FallbackVerifier fv = new FallbackVerifier(address(fG));
        console.log("  FallbackVerifier (adapter):", address(fv));

        LayerHashesGroth16VerifierGenerated lG = new LayerHashesGroth16VerifierGenerated();
        console.log("  LayerHashesGroth16VerifierGenerated:", address(lG));
        LayerHashesMovementVerifier lv = new LayerHashesMovementVerifier(address(lG));
        console.log("  LayerHashesMovementVerifier (adapter):", address(lv));

        vb = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(pv)),
            fallbackVerifier: IFallbackVerifier(address(fv)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(lv)),
            genesisBkSetCommitment: genesisBkSetCommitment,
            genesisPrevMaxLevelLayerHash: genesisPrevAnchor
        });
        console.log("verifyBlock anchors:");
        console.log("  genesis BK-set commitment:", genesisBkSetCommitment);
        console.log("  genesis prev-anchor      :", genesisPrevAnchor);
    }

    /// Serialise the deployment result to `deployment_real.json`. Extracted
    /// out of `run()` for the same coverage stack-depth reason as
    /// `_buildVerifyBlockConfig`. The verifier-triple fields are emitted
    /// only when `wireVerifyBlock == true` so the disabled-mode JSON is
    /// shape-compatible with the pre-E4 (2026-05-19) script output.
    function _writeDeploymentJson(DeploymentJsonArgs memory a) internal {
        string memory verifierJson = "";
        if (a.wireVerifyBlock) {
            verifierJson = string(
                abi.encodePacked(
                    '  "primary_verifier": "',
                    vm.toString(a.primaryVerifierAddr),
                    '",\n',
                    '  "fallback_verifier": "',
                    vm.toString(a.fallbackVerifierAddr),
                    '",\n',
                    '  "layer_hashes_verifier": "',
                    vm.toString(a.layerHashesVerifierAddr),
                    '",\n',
                    '  "genesis_bk_set_commitment": "',
                    vm.toString(a.genesisBkSetCommitment),
                    '",\n',
                    '  "genesis_prev_max_level_layer_hash": "',
                    vm.toString(a.genesisPrevAnchor),
                    '",\n'
                )
            );
        }

        string memory deploymentJson = string(
            abi.encodePacked(
                "{\n",
                '  "oracle": "',
                vm.toString(a.oracleAddr),
                '",\n',
                '  "oracle_type": "',
                a.oracleType,
                '",\n',
                '  "bridge": "',
                vm.toString(a.bridgeAddr),
                '",\n',
                '  "verify_block_wired": ',
                a.wireVerifyBlock ? "true" : "false",
                ",\n",
                verifierJson,
                '  "aave_enabled": ',
                a.useAave ? "true" : "false",
                "\n}"
            )
        );

        vm.writeFile("deployment_real.json", deploymentJson);
    }
}
