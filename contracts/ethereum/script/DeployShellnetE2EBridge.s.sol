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

    function run() external {
        uint256 genesisBk = vm.envUint("GENESIS_BK_SET_COMMITMENT");
        uint256 genesisPrev = vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH");
        uint256 withdrawDappFr = vm.envOr("WITHDRAW_DAPP_FR", uint256(0));
        uint256 withdrawAccFr = vm.envUint("WITHDRAW_ACC_FR");
        uint256 altDstChainId = vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(1));
        uint256 altTokenId = vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(3));

        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle:", address(oracle));

        PrimaryGroth16VerifierGenerated pG = new PrimaryGroth16VerifierGenerated();
        PrimaryVerifier pv = new PrimaryVerifier(address(pG));
        FallbackGroth16VerifierGenerated fG = new FallbackGroth16VerifierGenerated();
        FallbackVerifier fv = new FallbackVerifier(address(fG));
        LayerHashesGroth16VerifierGenerated lG = new LayerHashesGroth16VerifierGenerated();
        LayerHashesMovementVerifier lv = new LayerHashesMovementVerifier(address(lG));

        MockBridgeWithdrawalVerifier mockWithdraw = new MockBridgeWithdrawalVerifier();
        mockWithdraw.setShouldAccept(true);
        console.log("MockBridgeWithdrawalVerifier:", address(mockWithdraw));

        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            USDC_SEPOLIA,
            address(0),
            address(0),
            AckiNackiBridge.VerifyBlockConfig({
                primaryVerifier: IPrimaryVerifier(address(pv)),
                fallbackVerifier: IFallbackVerifier(address(fv)),
                layerHashesVerifier: ILayerHashesMovementVerifier(address(lv)),
                genesisBkSetCommitment: genesisBk,
                genesisPrevMaxLevelLayerHash: genesisPrev
            }),
            AckiNackiBridge.BridgeWithdrawConfig({
                bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(mockWithdraw)),
                dappFr: withdrawDappFr,
                accFr: withdrawAccFr,
                altDstChainId: altDstChainId,
                altTokenId: altTokenId
            })
        );

        vm.stopBroadcast();

        console.log("AckiNackiBridge (shellnet E2E):", address(bridge));
        console.log("USDC:", USDC_SEPOLIA);
        console.log("withdraw dappFr:", withdrawDappFr);
        console.log("withdraw accFr:", withdrawAccFr);
        console.log("altDstChainId:", altDstChainId);
        console.log("altTokenId:", altTokenId);
    }
}
