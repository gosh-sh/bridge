// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "./ShplonkDeployLib.sol";
import "./PredecessorEnv.sol";
import "./ArbitrumSepoliaAave.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for the production bridge contract.
 * @dev AN→ETH verifiers: R15 SHPLONK aggregators for 1A + 1B + 2 (1B hybrid was retired on e2a962b).
 *
 * Chain: Arbitrum Sepolia (421614) only; any other chain id reverts. The
 * bridge always gets the AAVE V3 USDC market of that chain — the pool and
 * aToken constants of `ArbitrumSepoliaAave`, with no override.
 *
 * Oracle: MockBlockHeaderOracle. USE_AXIOM_ORACLE=true reverts: Axiom has no
 * deployment on Arbitrum.
 *
 * verifyBlock wiring:
 *   - WIRE_VERIFY_BLOCK=true → deploy Primary/Fallback/LayerHashes Shplonk adapters.
 *     Requires GENESIS_BK_SET_COMMITMENT (non-zero) and GENESIS_PREV_MAX_LEVEL_LAYER_HASH.
 *     Requires `verifiers/PrimaryAggregatorVerifier.bin` + `FallbackAggregatorVerifier.bin`
 *     + `LayerHashesAggregatorVerifier.bin` (or SHPLONK_BIN_PRIMARY / SHPLONK_BIN_FALLBACK /
 *     SHPLONK_BIN_LAYER_HASHES). All three circuits use the R15 SHPLONK aggregator path.
 *
 * withdrawByProof wiring:
 *   - MANDATORY on the production script (NB-Q8): the C4 aggregator `.bin` is
 *     committed and the Yul adapter is production-ready, so silently shipping
 *     with `address(0)` here bricks user withdrawals. Requires WITHDRAW_ACC_FR
 *     (non-zero). Optional: WITHDRAW_DAPP_FR, alt dst/token ids. This
 *     script always wires Circuit 4, so WIRE_VERIFY_BLOCK=true is required on
 *     every chain (`WithdrawRequiresVerifyBlock`). VerifyBlock-only bring-up:
 *     DeployShellnetE2EBridge on local anvil.
 *
 * Predecessor:
 *   - PREDECESSOR_BRIDGE (optional, zero by default): the bridge on this
 *     chain whose treasury this one may take over (`migrateTreasury` /
 *     `fundTreasury`).
 */
contract DeployRealBridge is Script {
    struct WireInputs {
        bool wireVerifyBlock;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
        uint256 withdrawDappFr;
        uint256 withdrawAccFr;
        uint256 altDstChainId;
        uint256 altDstHostChainId;
        uint256 altTokenId;
        address predecessor;
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
        bool wireVerifyBlock;
        address primaryVerifierAddr;
        address fallbackVerifierAddr;
        address layerHashesVerifierAddr;
        address withdrawalVerifierAddr;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevAnchor;
        address predecessor;
    }

    function run() external {
        deploy();
    }

    function deploy() public returns (AckiNackiBridge bridge) {
        require(
            block.chainid == ArbitrumSepoliaAave.CHAIN_ID,
            "DeployRealBridge: Arbitrum Sepolia (421614) only"
        );
        require(
            !vm.envOr("USE_AXIOM_ORACLE", false), "Axiom oracle not supported on Arbitrum Sepolia"
        );
        WireInputs memory w = _readWireInputs();
        vm.startBroadcast(vm.envUint("PRIVATE_KEY"));

        address oracleAddr = address(new MockBlockHeaderOracle());
        string memory oracleType = "MockBlockHeaderOracle";
        console.log("MockBlockHeaderOracle deployed at:", oracleAddr);
        DeployResult memory r = _deployBridge(oracleAddr, w);

        vm.stopBroadcast();

        _logSummary(oracleType, oracleAddr, r, w);

        _writeDeploymentJson(
            DeploymentJsonArgs({
                oracleAddr: oracleAddr,
                oracleType: oracleType,
                bridgeAddr: r.bridgeAddr,
                wireVerifyBlock: w.wireVerifyBlock,
                primaryVerifierAddr: r.primaryVerifierAddr,
                fallbackVerifierAddr: r.fallbackVerifierAddr,
                layerHashesVerifierAddr: r.layerHashesVerifierAddr,
                withdrawalVerifierAddr: r.withdrawalVerifierAddr,
                genesisBkSetCommitment: w.genesisBkSetCommitment,
                genesisPrevAnchor: w.genesisPrevAnchor,
                predecessor: w.predecessor
            })
        );
        console.log("\nDeployment info saved to: deployment_real.json");
        bridge = AckiNackiBridge(r.bridgeAddr);
    }

    function _readWireInputs() internal view returns (WireInputs memory w) {
        w.wireVerifyBlock = vm.envOr("WIRE_VERIFY_BLOCK", false);
        // NB-Q8 wires Circuit 4 on this script. That is illegal without the
        // verifyBlock triple (empty windows → every withdraw UnknownAnchor).
        require(w.wireVerifyBlock, "DeployRealBridge requires WIRE_VERIFY_BLOCK=true");

        if (w.wireVerifyBlock) {
            w.genesisBkSetCommitment = vm.envUint("GENESIS_BK_SET_COMMITMENT");
            w.genesisPrevAnchor = vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH");
            require(w.genesisBkSetCommitment != 0, "GENESIS_BK_SET_COMMITMENT required");
        }
        // NB-Q8: withdrawByProof wiring is mandatory on RealBridge. The C4 `.bin`
        // is committed and the Yul adapter is production-ready — a deploy that
        // silently ships with `address(0)` bricks user withdrawals.
        w.withdrawAccFr = vm.envUint("WITHDRAW_ACC_FR");
        require(w.withdrawAccFr != 0, "WITHDRAW_ACC_FR required (NB-Q8: withdraw wiring mandatory)");
        w.withdrawDappFr = vm.envOr("WITHDRAW_DAPP_FR", uint256(0));
        w.altDstChainId = vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(0));
        w.altDstHostChainId = vm.envOr("WITHDRAW_ALT_DST_HOST_CHAIN_ID", uint256(0));
        w.altTokenId = vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(0));
        w.predecessor = PredecessorEnv.read();
    }

    function _deployBridge(address oracleAddr, WireInputs memory w)
        internal
        returns (DeployResult memory r)
    {
        AckiNackiBridge.VerifyBlockConfig memory vb = _buildVerifyBlockConfig(
            w.wireVerifyBlock, w.genesisBkSetCommitment, w.genesisPrevAnchor
        );
        AckiNackiBridge.BridgeWithdrawConfig memory bw = _buildWithdrawConfig(
            w.withdrawDappFr,
            w.withdrawAccFr,
            w.altDstChainId,
            w.altDstHostChainId,
            w.altTokenId,
            w.predecessor
        );

        if (w.wireVerifyBlock) {
            r.primaryVerifierAddr = address(vb.primaryVerifier);
            r.fallbackVerifierAddr = address(vb.fallbackVerifier);
            r.layerHashesVerifierAddr = address(vb.layerHashesVerifier);
        }
        r.withdrawalVerifierAddr = address(bw.bridgeWithdrawalVerifier);

        console.log("Deploying AckiNackiBridge (Shplonk verifiers only)...");
        AckiNackiBridge bridge = new AckiNackiBridge(
            oracleAddr,
            ArbitrumSepoliaAave.USDC,
            ArbitrumSepoliaAave.POOL,
            ArbitrumSepoliaAave.AUSDC,
            vb,
            bw
        );
        r.bridgeAddr = address(bridge);

        console.log("AckiNackiBridge deployed at:", r.bridgeAddr);
    }

    function _logSummary(
        string memory oracleType,
        address oracleAddr,
        DeployResult memory r,
        WireInputs memory w
    ) internal pure {
        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", r.bridgeAddr);
        if (w.wireVerifyBlock) {
            console.log("PrimaryAggregatorVerifier:", r.primaryVerifierAddr);
            console.log("FallbackAggregatorVerifier:", r.fallbackVerifierAddr);
            console.log("LayerHashesAggregatorVerifier:", r.layerHashesVerifierAddr);
        }
        console.log("BridgeWithdrawalAggregatorVerifier:", r.withdrawalVerifierAddr);
        console.log("Predecessor:", w.predecessor);
        console.log("AAVE pool:", ArbitrumSepoliaAave.POOL);
        console.log("aUSDC:", ArbitrumSepoliaAave.AUSDC);
        if (!w.wireVerifyBlock) {
            console.log("\n  ** verifyBlock DISABLED - WIRE_VERIFY_BLOCK + Shplonk .bin **");
        }
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
                    genesisPrevMaxLevelLayerHash: 0,
                    genesisLastSeenBlockSeqNo: 0,
                    bundleStride: 0,
                    genesisLastBkSetUpdateSeqNo: 0
                });
        }

        console.log("Deploying production verifyBlock triple (1A/1B/2 Shplonk)...");
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockProductionFromEnv();

        // `BUNDLE_STRIDE` MUST match the daemon's `AnchorMode::stride()`
        // for this deployment — `W·P` for L1, `W²` for L2. Required for
        // production verifyBlock wiring (reverts with `InvalidBundleStride`
        // otherwise).
        vb = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: v.primary,
            fallbackVerifier: v.fallback_,
            layerHashesVerifier: v.layerHashes,
            genesisBkSetCommitment: genesisBkSetCommitment,
            genesisPrevMaxLevelLayerHash: genesisPrevAnchor,
            genesisLastSeenBlockSeqNo: uint64(
                vm.envOr("GENESIS_LAST_SEEN_BLOCK_SEQNO", uint256(0))
            ),
            bundleStride: uint64(vm.envUint("BUNDLE_STRIDE")),
            genesisLastBkSetUpdateSeqNo: uint64(
                vm.envOr("GENESIS_LAST_BK_SET_UPDATE_SEQNO", uint256(0))
            )
        });
        console.log("  PrimaryAggregatorVerifier:", address(v.primary));
        console.log("  FallbackAggregatorVerifier:", address(v.fallback_));
        console.log("  LayerHashesAggregatorVerifier:", address(v.layerHashes));
    }

    function _buildWithdrawConfig(
        uint256 dappFr,
        uint256 accFr,
        uint256 altDstChainId,
        uint256 altDstHostChainId,
        uint256 altTokenId,
        address predecessor
    ) internal returns (AckiNackiBridge.BridgeWithdrawConfig memory bw) {
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
            altTokenId: altTokenId,
            predecessor: IPredecessorBridge(predecessor)
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
        verifierJson = string(
            abi.encodePacked(
                verifierJson,
                '  "withdrawal_verifier": "',
                vm.toString(a.withdrawalVerifierAddr),
                '",\n',
                '  "predecessor": "',
                vm.toString(a.predecessor),
                '",\n'
            )
        );

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
                '  "withdraw_wired": true,\n',
                verifierJson,
                '  "aave_pool": "',
                vm.toString(ArbitrumSepoliaAave.POOL),
                '",\n',
                '  "aave_ausdc": "',
                vm.toString(ArbitrumSepoliaAave.AUSDC),
                '",\n',
                '  "aave_enabled": true',
                "\n}"
            )
        );

        vm.writeFile("deployment_real.json", deploymentJson);
    }
}
