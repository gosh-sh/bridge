// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/Bn254FrLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title VerifyBlockCEITest
/// @notice Phase C / A2 — A2-INV-1 / AC-6: no state mutation on crypto rejection.
contract VerifyBlockCEITest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;

    function setUp() public {
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _submit(1);
    }

    function _layers(uint256 idx) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            arr[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("cei", idx, i))));
        }
    }

    function _submit(uint256 idx) internal {
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0x2000 + idx,
            BK_SET,
            uint64(idx),
            ACTIVE_LAYERS,
            _layers(idx),
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    function _snapshot() internal view returns (uint64 seq, uint256 l1, bool known) {
        seq = bridge.storedLastSeenBlockSeqNo();
        l1 = bridge.getLatestPerLayer()[0];
        known = bridge.isKnownLayerAnchor(1, l1);
    }

    function test_verifyBlock_attestationReject_stateUntouched() public {
        (uint64 seqBefore, uint256 l1Before, bool knownBefore) = _snapshot();

        primaryVerifier.setShouldAccept(false);
        uint256 anchor = bridge.expectedPrevAnchor(ACTIVE_LAYERS);
        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"03",
            hex"04",
            0x3000,
            BK_SET,
            2,
            ACTIVE_LAYERS,
            _layers(2),
            anchor
        );

        (uint64 seqAfter, uint256 l1After, bool knownAfter) = _snapshot();
        assertEq(seqAfter, seqBefore, "seq");
        assertEq(l1After, l1Before, "stored L1");
        assertEq(knownAfter, knownBefore, "anchor window");
    }

    function test_verifyBlock_layerHashesReject_stateUntouched() public {
        (uint64 seqBefore, uint256 l1Before, bool knownBefore) = _snapshot();

        layerHashesVerifier.setShouldAccept(false);
        uint256 anchor = bridge.expectedPrevAnchor(ACTIVE_LAYERS);
        vm.expectRevert(AckiNackiBridge.LayerHashesProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"05",
            hex"06",
            0x4000,
            BK_SET,
            2,
            ACTIVE_LAYERS,
            _layers(2),
            anchor
        );

        (uint64 seqAfter, uint256 l1After, bool knownAfter) = _snapshot();
        assertEq(seqAfter, seqBefore, "seq");
        assertEq(l1After, l1Before, "stored L1");
        assertEq(knownAfter, knownBefore, "anchor window");
    }
}
