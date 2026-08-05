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

/// @title DeployShellnetE2EBridge
/// @notice Sepolia deploy for shellnet AN→ETH E2E: SHPLONK aggregators for 1A/1B/2,
///         SHPLONK for C4 when wired.
/// @dev Requires `verifiers/PrimaryAggregatorVerifier.bin` +
///      `verifiers/FallbackAggregatorVerifier.bin` + `verifiers/LayerHashesAggregatorVerifier.bin`
///      (or `SHPLONK_BIN_*` overrides).
///
///      withdrawByProof (Circuit 4) wiring is OFF by default — set `WIRE_WITHDRAW_BY_PROOF=true`
///      (and provide `WITHDRAW_ACC_FR` + `verifiers/BridgeWithdrawalAggregatorVerifier.bin`) once
///      partner M4 lands. Until then this script deploys a verifyBlock-only bridge so it
///      does not depend on the not-yet-existing C4 `.bin`.
contract DeployShellnetE2EBridge is Script {
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;

    struct VerifyBlockWiring {
        IPrimaryVerifier primary;
        IFallbackVerifier fallback_;
        ILayerHashesMovementVerifier layerHashes;
        uint256 genesisBkSetCommitment;
        uint256 genesisPrevMaxLevelLayerHash;
    }

    struct WithdrawWiring {
        IBridgeWithdrawalVerifier verifier;
        uint256 dappFr;
        uint256 accFr;
        uint256 altDstChainId;
        uint256 altDstHostChainId;
        uint256 altTokenId;
    }

    function run() external {
        VerifyBlockWiring memory vb = VerifyBlockWiring({
            primary: IPrimaryVerifier(address(0)),
            fallback_: IFallbackVerifier(address(0)),
            layerHashes: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: vm.envUint("GENESIS_BK_SET_COMMITMENT"),
            genesisPrevMaxLevelLayerHash: vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH")
        });
        bool wireWithdraw = vm.envOr("WIRE_WITHDRAW_BY_PROOF", false);
        WithdrawWiring memory wd = WithdrawWiring({
            verifier: IBridgeWithdrawalVerifier(address(0)),
            dappFr: wireWithdraw ? vm.envOr("WITHDRAW_DAPP_FR", uint256(0)) : uint256(0),
            accFr: wireWithdraw ? vm.envUint("WITHDRAW_ACC_FR") : uint256(0),
            altDstChainId: wireWithdraw
                ? vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(1))
                : uint256(0),
            altDstHostChainId: wireWithdraw
                ? vm.envOr("WITHDRAW_ALT_DST_HOST_CHAIN_ID", uint256(11_155_111))
                : uint256(0),
            altTokenId: wireWithdraw ? vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(3)) : uint256(0)
        });
        if (wireWithdraw) {
            require(wd.accFr != 0, "WITHDRAW_ACC_FR required for Shplonk C4 wiring");
        }

        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle:", address(oracle));

        (vb.primary, vb.fallback_, vb.layerHashes) = _deployProductionVerifyBlockTriple();
        console.log("PrimaryAggregatorVerifier:", address(vb.primary));
        console.log("FallbackAggregatorVerifier:", address(vb.fallback_));
        console.log("LayerHashesAggregatorVerifier:", address(vb.layerHashes));

        if (wireWithdraw) {
            wd.verifier =
                ShplonkDeployLib.deployWithdrawalAdapter(ShplonkDeployLib.withdrawalBinPath());
            console.log("BridgeWithdrawalAggregatorVerifier:", address(wd.verifier));
        } else {
            console.log("withdrawByProof DISABLED - set WIRE_WITHDRAW_BY_PROOF=true + C4 .bin (M4)");
        }

        AckiNackiBridge bridge = _deployBridge(address(oracle), vb, wd);

        vm.stopBroadcast();

        console.log("AckiNackiBridge (shellnet E2E):", address(bridge));
        console.log("USDC:", USDC_SEPOLIA);
        console.log("withdrawWired:", wireWithdraw);
        console.log("withdraw dappFr:", wd.dappFr);
        console.log("withdraw accFr:", wd.accFr);
        console.log("altDstChainId:", wd.altDstChainId);
        console.log("altDstHostChainId:", wd.altDstHostChainId);
        console.log("altTokenId:", wd.altTokenId);
    }

    function _deployProductionVerifyBlockTriple()
        internal
        returns (IPrimaryVerifier, IFallbackVerifier, ILayerHashesMovementVerifier)
    {
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockProductionFromEnv();
        return (v.primary, v.fallback_, v.layerHashes);
    }

    function _deployBridge(address oracle, VerifyBlockWiring memory vb, WithdrawWiring memory wd)
        internal
        returns (AckiNackiBridge)
    {
        return new AckiNackiBridge(
            oracle,
            USDC_SEPOLIA,
            address(0),
            address(0),
            AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: vb.primary,
                fallbackVerifier: vb.fallback_,
                layerHashesVerifier: vb.layerHashes,
                genesisBkSetCommitment: vb.genesisBkSetCommitment,
                genesisPrevMaxLevelLayerHash: vb.genesisPrevMaxLevelLayerHash,
                genesisLastSeenBlockSeqNo: uint64(vm.envOr("GENESIS_LAST_SEEN_BLOCK_SEQNO", uint256(0)))
            }),
            AckiNackiBridge.BridgeWithdrawConfig({
                bridgeWithdrawalVerifier: wd.verifier,
                dappFr: wd.dappFr,
                accFr: wd.accFr,
                altDstChainId: wd.altDstChainId,
                altDstHostChainId: wd.altDstHostChainId,
                altTokenId: wd.altTokenId
            })
        );
    }
}
