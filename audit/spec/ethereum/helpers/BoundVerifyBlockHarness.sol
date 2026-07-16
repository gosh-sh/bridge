// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/PrimaryVerifier.sol";
import "@src/PrimaryGroth16VerifierGenerated.sol";
import "@src/LayerHashesMovementVerifier.sol";
import "@src/LayerHashesGroth16VerifierGenerated.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @dev Bound Groth16 verifyBlock rig for audit E-03 cross-circuit negatives.
abstract contract BoundVerifyBlockHarness is Test {
    uint256 internal constant BLOCK_ID =
        20322406600520462375758092294219252768443831515974743492093184796387680257654;
    uint256 internal constant BK_SET_POSEIDON =
        12131685556123040013370001299379043353566086228347545885725494982201367766590;
    uint64 internal constant BLOCK_SEQ_NO = 1;
    uint8 internal constant NUM_LAYERS = 5;
    uint256 internal constant PREV_MAX_LEVEL_LAYER_HASH =
        428496435308882603647236216064516127915391730646288063657718627407097714118;

    bytes internal constant PROOF_PRIMARY =
        hex"250e54dc356d7a3e75a001445d8e68161026769c256e24ee2a98989fe3b053cf0cab97566d6da19f08ecc0f80f8b4fc07c2237c66bde30d86772a27a28e0c36d0d3040adb45f17b39c3770752fb313e6c5197fd62888e53f234488f5d2345c05018bb679a901cf6e7941fa51eca8eae4fb096550384f09a5daf5056d3626797a1f366f5b7d5a8092bc38f3733682929023b13aa4cb5acf9d012f2454f08b13b52e397c247d57ad5f7428dad9b1138b16194591e9c3083b11146220258df92c3518ec536d04b52fbdc6d68f3e68b8e9f99fd489ee76e698d1bf00c6d1def9d01403dfc6047d2fdd625739e75cd08063cb5502e04e39c74d667ed44c5290bdda7b";

    bytes internal constant PROOF_LAYER_HASHES =
        hex"1f5fa2faf1f9332e6794fa01ff88ee2e8f1638633573b152f13e922bce2964ce255e3654cd95b3d59487fa7287da197f6cbd4a905f3f2cb3d92ad4dd32c9682e09316a5373289b332399f5db8bfe0e9537778f3a7b1f17a092d2047423eba1863012f5546d7730e009b0aa1dca6252252b033944e201fdc22ad7401a4713b45d2669e67b2c106c75bf765fe78a6a9cfbfb949b5ac16fd7cc91448677e3be07f709077287569a62617c398085a233da6b2598bdc0e9fcb3cf653ced49a9d808ed1ea221ef5fedff54bdb7f6bc1595857c6d00cffc0b416acff8db68d075ff78a206cb9d3d5717ecab99303bbe4ac69eb4b08ea4da4d1e1e4057ebe09db8b0bf98";

    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    PrimaryVerifier internal primaryVerifier;
    LayerHashesMovementVerifier internal layerHashesVerifier;
    MockFallbackVerifier internal fallbackVerifier;

    function _layerHashes() internal pure returns (uint256[10] memory arr) {
        arr[0] = 5597430690756036137200850231008677009392496962870610452683763762890050825836;
        arr[1] = 4796295659576234412953062246444144483845500275027135395768740734699524433822;
        arr[2] = 13612500885536344885003699466285227870961224105616882696060905946370081269316;
        arr[3] = 765644099832424376728543246025162156692809455668879575312335534521837264383;
        arr[4] = 11258276999963302388849994778928696710880810094012346484732501452243136409891;
    }

    function setUpBoundVerifyBlockBridge() internal {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);

        PrimaryGroth16VerifierGenerated primaryGroth16 = new PrimaryGroth16VerifierGenerated();
        primaryVerifier = new PrimaryVerifier(address(primaryGroth16));
        LayerHashesGroth16VerifierGenerated layerGroth16 = new LayerHashesGroth16VerifierGenerated();
        layerHashesVerifier = new LayerHashesMovementVerifier(address(layerGroth16));
        fallbackVerifier = new MockFallbackVerifier();

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primaryVerifier)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layerHashesVerifier)),
            BK_SET_POSEIDON,
            PREV_MAX_LEVEL_LAYER_HASH
        );

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            vb,
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }
}
