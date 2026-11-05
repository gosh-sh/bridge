// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "./PredecessorEnv.sol";
import "./ShplonkDeployLib.sol";
import "./ArbitrumSepoliaAave.sol";
import "../test/mocks/MockAave.sol";

/// @title DeployShellnetE2EBridge
/// @notice Arbitrum Sepolia deploy for shellnet AN→ETH E2E: SHPLONK aggregators
///         for 1A/1B/2, SHPLONK for C4 when wired.
/// @dev Requires `verifiers/PrimaryAggregatorVerifier.bin` +
///      `verifiers/FallbackAggregatorVerifier.bin` + `verifiers/LayerHashesAggregatorVerifier.bin`
///      (or `SHPLONK_BIN_*` overrides).
///
///      withdrawByProof (Circuit 4) wiring is on by default and MUST stay on for any
///      chain other than local anvil (chainid 31337). The C4 `.bin` is committed and the
///      Yul adapter is production-ready — silently shipping with `address(0)` on Sepolia
///      or any other real chain bricks user withdrawals (NB-Q8). The `WIRE_WITHDRAW_BY_PROOF`
///      env var is kept only so CI runs against a fresh anvil can opt out; setting it to
///      `false` off anvil reverts.
///
///      `PREDECESSOR_BRIDGE` is optional: the bridge on this chain whose
///      treasury this one may take over (`PredecessorEnv`).
///
///      AAVE: on 421614 the bridge gets Circle's USDC and the market of
///      `ArbitrumSepoliaAave`, with no override; `USDC_ADDRESS`, if set, must
///      be that USDC. On local anvil (31337) `USDC_ADDRESS` is required and a
///      mock market is deployed for it. Any other chain reverts.
contract DeployShellnetE2EBridge is Script {
    /// USDC and the AAVE market the bridge is deployed against.
    function _market() internal returns (address usdc, address pool, address aUsdc) {
        address fromEnv = vm.envOr("USDC_ADDRESS", address(0));
        if (block.chainid == ArbitrumSepoliaAave.CHAIN_ID) {
            require(
                fromEnv == address(0) || fromEnv == ArbitrumSepoliaAave.USDC,
                "USDC_ADDRESS on 421614 must be the AAVE market's USDC"
            );
            return (ArbitrumSepoliaAave.USDC, ArbitrumSepoliaAave.POOL, ArbitrumSepoliaAave.AUSDC);
        }
        require(block.chainid == 31337, "DeployShellnetE2EBridge: Arbitrum Sepolia or anvil only");
        require(fromEnv != address(0), "USDC_ADDRESS required on anvil");
        MockAUSDC aToken = new MockAUSDC();
        MockAavePool mockPool = new MockAavePool(fromEnv, address(aToken));
        return (fromEnv, address(mockPool), address(aToken));
    }

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
        deploy();
    }

    function deploy() public returns (AckiNackiBridge bridge) {
        VerifyBlockWiring memory vb = VerifyBlockWiring({
            primary: IPrimaryVerifier(address(0)),
            fallback_: IFallbackVerifier(address(0)),
            layerHashes: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: vm.envUint("GENESIS_BK_SET_COMMITMENT"),
            genesisPrevMaxLevelLayerHash: vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH")
        });
        // NB-Q8: default ON; `false` only tolerated on local anvil (chainid 31337).
        bool wireWithdraw = vm.envOr("WIRE_WITHDRAW_BY_PROOF", true);
        require(
            wireWithdraw || block.chainid == 31337,
            "WIRE_WITHDRAW_BY_PROOF=false only allowed on anvil (chainid 31337)"
        );
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

        bridge = _deployBridge(address(oracle), vb, wd);

        vm.stopBroadcast();

        console.log("AckiNackiBridge (shellnet E2E):", address(bridge));
        console.log("USDC:", address(bridge.usdc()));
        console.log("AAVE pool:", address(bridge.aavePool()));
        console.log("aUSDC:", address(bridge.aUSDC()));
        console.log("withdrawWired:", wireWithdraw);
        console.log("withdraw dappFr:", wd.dappFr);
        console.log("withdraw accFr:", wd.accFr);
        console.log("altDstChainId:", wd.altDstChainId);
        console.log("altDstHostChainId:", wd.altDstHostChainId);
        console.log("altTokenId:", wd.altTokenId);
        console.log("predecessor:", address(bridge.predecessor()));
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
        (address usdc, address pool, address aUsdc) = _market();
        return new AckiNackiBridge(
            oracle,
            usdc,
            pool,
            aUsdc,
            AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: vb.primary,
                fallbackVerifier: vb.fallback_,
                layerHashesVerifier: vb.layerHashes,
                genesisBkSetCommitment: vb.genesisBkSetCommitment,
                genesisPrevMaxLevelLayerHash: vb.genesisPrevMaxLevelLayerHash,
                genesisLastSeenBlockSeqNo: uint64(
                    vm.envOr("GENESIS_LAST_SEEN_BLOCK_SEQNO", uint256(0))
                ),
                bundleStride: uint64(vm.envUint("BUNDLE_STRIDE")),
                genesisLastBkSetUpdateSeqNo: uint64(
                    vm.envOr("GENESIS_LAST_BK_SET_UPDATE_SEQNO", uint256(0))
                )
            }),
            AckiNackiBridge.BridgeWithdrawConfig({
                bridgeWithdrawalVerifier: wd.verifier,
                dappFr: wd.dappFr,
                accFr: wd.accFr,
                altDstChainId: wd.altDstChainId,
                altDstHostChainId: wd.altDstHostChainId,
                altTokenId: wd.altTokenId,
                predecessor: IPredecessorBridge(PredecessorEnv.read())
            })
        );
    }
}
