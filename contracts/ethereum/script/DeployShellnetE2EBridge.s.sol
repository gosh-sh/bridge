// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/PrimaryGroth16VerifierGenerated.sol";
import "../src/FallbackGroth16VerifierGenerated.sol";
import "../src/LayerHashesGroth16VerifierGenerated.sol";
import "../src/PrimaryVerifier.sol";
import "../src/FallbackVerifier.sol";
import "../src/LayerHashesMovementVerifier.sol";
import "../test/mocks/MockBridgeWithdrawalVerifier.sol";

/// @title DeployShellnetE2EBridge
/// @notice Sepolia deploy for the shellnet AN→ETH E2E: `verifyBlock` wired +
///         mock Circuit 4 verifier with shellnet `(dappFr, accFr, dstChainId,
///         tokenId)` aliases.
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
        WithdrawWiring memory wd = WithdrawWiring({
            verifier: IBridgeWithdrawalVerifier(address(0)),
            dappFr: vm.envOr("WITHDRAW_DAPP_FR", uint256(0)),
            accFr: vm.envUint("WITHDRAW_ACC_FR"),
            altDstChainId: vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(1)),
            altTokenId: vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(3))
        });

        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle:", address(oracle));

        (vb.primary, vb.fallback_, vb.layerHashes) = _deployVerifyBlockVerifiers();
        wd.verifier = _deployMockWithdrawVerifier();

        AckiNackiBridge bridge = _deployBridge(address(oracle), vb, wd);

        vm.stopBroadcast();

        console.log("AckiNackiBridge (shellnet E2E):", address(bridge));
        console.log("USDC:", USDC_SEPOLIA);
        console.log("withdraw dappFr:", wd.dappFr);
        console.log("withdraw accFr:", wd.accFr);
        console.log("altDstChainId:", wd.altDstChainId);
        console.log("altTokenId:", wd.altTokenId);
    }

    function _deployVerifyBlockVerifiers()
        internal
        returns (IPrimaryVerifier, IFallbackVerifier, ILayerHashesMovementVerifier)
    {
        PrimaryGroth16VerifierGenerated pG = new PrimaryGroth16VerifierGenerated();
        PrimaryVerifier pv = new PrimaryVerifier(address(pG));
        FallbackGroth16VerifierGenerated fG = new FallbackGroth16VerifierGenerated();
        FallbackVerifier fv = new FallbackVerifier(address(fG));
        LayerHashesGroth16VerifierGenerated lG = new LayerHashesGroth16VerifierGenerated();
        LayerHashesMovementVerifier lv = new LayerHashesMovementVerifier(address(lG));
        return (
            IPrimaryVerifier(address(pv)),
            IFallbackVerifier(address(fv)),
            ILayerHashesMovementVerifier(address(lv))
        );
    }

    function _deployMockWithdrawVerifier() internal returns (IBridgeWithdrawalVerifier) {
        MockBridgeWithdrawalVerifier mockWithdraw = new MockBridgeWithdrawalVerifier();
        mockWithdraw.setShouldAccept(true);
        console.log("MockBridgeWithdrawalVerifier:", address(mockWithdraw));
        return IBridgeWithdrawalVerifier(address(mockWithdraw));
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
                genesisPrevMaxLevelLayerHash: vb.genesisPrevMaxLevelLayerHash
            }),
            AckiNackiBridge.BridgeWithdrawConfig({
                bridgeWithdrawalVerifier: wd.verifier,
                dappFr: wd.dappFr,
                accFr: wd.accFr,
                altDstChainId: wd.altDstChainId,
                altTokenId: wd.altTokenId
            })
        );
    }
}
