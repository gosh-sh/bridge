// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/AxiomBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "./ShplonkDeployLib.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for the production bridge contract.
 * @dev AN→ETH verifiers: R15 SHPLONK aggregators for 1A + 2; gnark Groth16 for 1B fallback.
 *
 * Oracle mode:
 *   - USE_AXIOM_ORACLE=true → AxiomBlockHeaderOracle (production)
 *   - default → MockBlockHeaderOracle (testing only)
 *
 * verifyBlock wiring:
 *   - WIRE_VERIFY_BLOCK=true → deploy Primary/Fallback/LayerHashes Shplonk adapters.
 *     Requires GENESIS_BK_SET_COMMITMENT (non-zero) and GENESIS_PREV_MAX_LEVEL_LAYER_HASH.
 *     Requires `verifiers/PrimaryAggregatorVerifier.bin` + `FallbackAggregatorVerifier.bin`
 *     + `LayerHashesAggregatorVerifier.bin` (or SHPLONK_BIN_PRIMARY / SHPLONK_BIN_FALLBACK /
 *     SHPLONK_BIN_LAYER_HASHES). All three circuits use the R15 SHPLONK aggregator path.
 *
 * withdrawByProof wiring:
 *   - WIRE_WITHDRAW_BY_PROOF=true → BridgeWithdrawalAggregatorVerifier + identity env vars.
 *     Requires WITHDRAW_ACC_FR (non-zero). Optional: WITHDRAW_DAPP_FR, alt dst/token ids.
 */
contract DeployRealBridge is Script {
    struct WireInputs {
        bool useAave;
        bool wireVerifyBlock;
        bool wireWithdraw;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
        uint256 withdrawDappFr;
        uint256 withdrawAccFr;
        uint256 altDstChainId;
        uint256 altDstHostChainId;
        uint256 altTokenId;
    }

    struct DeployResult {
        address bridgeAddr;
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        address withdrawalVerifierAddr;
    }

    struct DeploymentJsonArgs {
        address oracleAddr;
        string oracleType;
        address bridgeAddr;
        bool useAave;
        bool wireVerifyBlock;
        bool wireWithdraw;
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        address withdrawalVerifierAddr;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
    }

    address constant AXIOM_V2_CORE_MAINNET = 0x69963768F8407dE501029680dE46945F838Fc98B;
    address constant AXIOM_V2_CORE_SEPOLIA = 0x69963768F8407dE501029680dE46945F838Fc98B;

    address constant AAVE_V3_POOL_MAINNET = 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2;
    address constant USDC_MAINNET = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
    address constant AAVE_V3_aUSDC_MAINNET = 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c;

    address constant AAVE_V3_POOL_SEPOLIA = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;
    address constant AAVE_V3_aUSDC_SEPOLIA = 0x16dA4541aD1807f4443d92D26044C1147406EB80;

    function run() external {
        bool useAxiomOracle = vm.envOr("USE_AXIOM_ORACLE", false);
        WireInputs memory w = _readWireInputs();

        vm.startBroadcast(vm.envUint("PRIVATE_KEY"));

        (address oracleAddr, string memory oracleType) = _deployOracle(useAxiomOracle);
        DeployResult memory r = _deployBridge(oracleAddr, w);

        vm.stopBroadcast();

        _logSummary(oracleType, oracleAddr, r, w, useAxiomOracle);

        _writeDeploymentJson(
            DeploymentJsonArgs({
                oracleAddr: oracleAddr,
                oracleType: oracleType,
                bridgeAddr: r.bridgeAddr,
                useAave: w.useAave,
                wireVerifyBlock: w.wireVerifyBlock,
                wireWithdraw: w.wireWithdraw,
                primaryVerifierAddr: r.primaryVerifierAddr,
                fallbackVerifierAddr: r.fallbackVerifierAddr,
                layerHashesVerifierAddr: r.layerHashesVerifierAddr,
                withdrawalVerifierAddr: r.withdrawalVerifierAddr,
                genesisBkSetCommitment: w.genesisBkSetCommitment,
                genesisPrevAnchor: w.genesisPrevAnchor
            })
        );
        console.log("\nDeployment info saved to: deployment_real.json");
    }

    function _readWireInputs() internal view returns (WireInputs memory w) {
        w.useAave = vm.envOr("USE_AAVE", false);
        w.wireVerifyBlock = vm.envOr("WIRE_VERIFY_BLOCK", false);
        w.wireWithdraw = vm.envOr("WIRE_WITHDRAW_BY_PROOF", false);

        if (w.wireVerifyBlock) {
            w.genesisBkSetCommitment = vm.envUint("GENESIS_BK_SET_COMMITMENT");
            w.genesisPrevAnchor = vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH");
            require(w.genesisBkSetCommitment != 0, "GENESIS_BK_SET_COMMITMENT required");
        }
        if (w.wireWithdraw) {
            w.withdrawAccFr = vm.envUint("WITHDRAW_ACC_FR");
            require(w.withdrawAccFr != 0, "WITHDRAW_ACC_FR required");
            w.withdrawDappFr = vm.envOr("WITHDRAW_DAPP_FR", uint256(0));
            w.altDstChainId = vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(0));
            w.altDstHostChainId = vm.envOr("WITHDRAW_ALT_DST_HOST_CHAIN_ID", uint256(0));
            w.altTokenId = vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(0));
        }
    }

    function _deployBridge(address oracleAddr, WireInputs memory w)
        internal
        returns (DeployResult memory r)
    {
        (address usdcAddr, address aavePool, address aUSDC) = _resolveTokenAddresses(w.useAave);

        AckiNackiBridge.VerifyBlockConfig memory vb = _buildVerifyBlockConfig(
            w.wireVerifyBlock, w.genesisBkSetCommitment, w.genesisPrevAnchor
        );
        AckiNackiBridge.BridgeWithdrawConfig memory bw = _buildWithdrawConfig(
            w.wireWithdraw,
            w.withdrawDappFr,
            w.withdrawAccFr,
            w.altDstChainId,
            w.altDstHostChainId,
            w.altTokenId
        );

        if (w.wireVerifyBlock) {
            r.primaryVerifierAddr = address(vb.primaryVerifier);
            r.fallbackVerifierAddr = address(vb.fallbackVerifier);
            r.layerHashesVerifierAddr = address(vb.layerHashesVerifier);
        }
        if (w.wireWithdraw) {
            r.withdrawalVerifierAddr = address(bw.bridgeWithdrawalVerifier);
        }

        console.log("Deploying AckiNackiBridge (Shplonk verifiers only)...");
        AckiNackiBridge bridge = new AckiNackiBridge(oracleAddr, usdcAddr, aavePool, aUSDC, vb, bw);
        r.bridgeAddr = address(bridge);

        console.log("AckiNackiBridge deployed at:", r.bridgeAddr);
    }

    function _logSummary(
        string memory oracleType,
        address oracleAddr,
        DeployResult memory r,
        WireInputs memory w,
        bool useAxiomOracle
    ) internal view {
        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", r.bridgeAddr);
        if (w.wireVerifyBlock) {
            console.log("PrimaryAggregatorVerifier:", r.primaryVerifierAddr);
            console.log("FallbackVerifier (Groth16):", r.fallbackVerifierAddr);
            console.log("LayerHashesAggregatorVerifier:", r.layerHashesVerifierAddr);
        }
        if (w.wireWithdraw) {
            console.log("BridgeWithdrawalAggregatorVerifier:", r.withdrawalVerifierAddr);
        }
        if (!useAxiomOracle) {
            console.log("\n  ** WARNING: MockBlockHeaderOracle - NOT for production **");
        }
        if (!w.wireVerifyBlock) {
            console.log("\n  ** verifyBlock DISABLED - WIRE_VERIFY_BLOCK + Shplonk .bin **");
        }
        if (!w.wireWithdraw) {
            console.log("\n  ** withdrawByProof DISABLED - WIRE_WITHDRAW_BY_PROOF + C4 .bin **");
        }
    }

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
                revert("Axiom oracle not supported on this chain");
            }
            oracleAddr = address(new AxiomBlockHeaderOracle(axiomV2Core));
            oracleType = "AxiomBlockHeaderOracle";
        } else {
            oracleAddr = address(new MockBlockHeaderOracle());
            oracleType = "MockBlockHeaderOracle";
        }
        console.log(string(abi.encodePacked(oracleType, " deployed at:")), oracleAddr);
    }

    function _resolveTokenAddresses(bool useAave)
        internal
        view
        returns (address usdcAddr, address aavePool, address aUSDC)
    {
        if (block.chainid == 1) {
            usdcAddr = USDC_MAINNET;
            if (useAave) {
                aavePool = AAVE_V3_POOL_MAINNET;
                aUSDC = AAVE_V3_aUSDC_MAINNET;
            }
            return (usdcAddr, aavePool, aUSDC);
        }
        if (block.chainid == 11155111) {
            usdcAddr = USDC_SEPOLIA;
            if (useAave) {
                aavePool = AAVE_V3_POOL_SEPOLIA;
                aUSDC = AAVE_V3_aUSDC_SEPOLIA;
            }
            return (usdcAddr, aavePool, aUSDC);
        }
        revert("USDC/AAVE wiring only supported on mainnet (1) or Sepolia (11155111)");
    }

    function _buildVerifyBlockConfig(
        bool wire,
        uint256 genesisBkSetCommitment,
        uint256 genesisPrevAnchor
    ) internal returns (AckiNackiBridge.VerifyBlockConfig memory vb) {
        if (!wire) {
            return AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: IPrimaryVerifier(address(0)),
                fallbackVerifier: IFallbackVerifier(address(0)),
                layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
                genesisBkSetCommitment: 0,
                genesisPrevMaxLevelLayerHash: 0
            });
        }

        console.log("Deploying production verifyBlock triple (1A/1B/2 Shplonk)...");
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockProductionFromEnv();

        vb = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: v.primary,
            fallbackVerifier: v.fallback_,
            layerHashesVerifier: v.layerHashes,
            genesisBkSetCommitment: genesisBkSetCommitment,
            genesisPrevMaxLevelLayerHash: genesisPrevAnchor
        });
        console.log("  PrimaryAggregatorVerifier:", address(v.primary));
        console.log("  FallbackAggregatorVerifier:", address(v.fallback_));
        console.log("  LayerHashesAggregatorVerifier:", address(v.layerHashes));
    }

    function _buildWithdrawConfig(
        bool wire,
        uint256 dappFr,
        uint256 accFr,
        uint256 altDstChainId,
        uint256 altDstHostChainId,
        uint256 altTokenId
    ) internal returns (AckiNackiBridge.BridgeWithdrawConfig memory bw) {
        if (!wire) {
            return AckiNackiBridge.BridgeWithdrawConfig({
                bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0)),
                dappFr: 0,
                accFr: 0,
                altDstChainId: 0,
                altDstHostChainId: 0,
                altTokenId: 0
            });
        }

        console.log("Deploying Shplonk BridgeWithdrawalAggregatorVerifier...");
        IBridgeWithdrawalVerifier w =
            ShplonkDeployLib.deployWithdrawalAdapter(ShplonkDeployLib.withdrawalBinPath());
        console.log("  BridgeWithdrawalAggregatorVerifier:", address(w));

        return AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: w,
            dappFr: dappFr,
            accFr: accFr,
            altDstChainId: altDstChainId,
            altDstHostChainId: altDstHostChainId,
            altTokenId: altTokenId
        });
    }

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
        if (a.wireWithdraw) {
            verifierJson = string(
                abi.encodePacked(
                    verifierJson,
                    '  "withdrawal_verifier": "',
                    vm.toString(a.withdrawalVerifierAddr),
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
                '  "withdraw_wired": ',
                a.wireWithdraw ? "true" : "false",
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
