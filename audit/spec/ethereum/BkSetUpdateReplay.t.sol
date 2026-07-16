// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title BkSetUpdateReplayTest
/// @notice Phase C / A2 — A2-INV-6 / BK-5: BK-set update replay rejected.
contract BkSetUpdateReplayTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    uint256 internal constant L3B = 0xB0C;
    uint64 internal constant SEQ = 42;

    function setUp() public {
        primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();

        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layer)),
                L2,
                0
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_applyBkSetUpdate_replaySameSeqNo_reverts() public {
        (uint256 blockId, bytes32 h0, bytes32 h23) = _merkleWitness(L2, L3);

        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            SEQ,
            L2,
            L3,
            h0,
            h23
        );
        assertEq(bridge.storedBkSetCommitment(), L3);

        (uint256 blockId2, bytes32 h0b, bytes32 h23b) = _merkleWitness(L2, L3B);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkUpdateSeqNoNotMonotonic.selector, SEQ, SEQ)
        );
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId2,
            SEQ,
            L3,
            L3B,
            h0b,
            h23b
        );

        assertEq(bridge.storedBkSetCommitment(), L3, "commitment unchanged on replay");
    }

    function _merkleWitness(uint256 l2, uint256 l3)
        internal
        pure
        returns (uint256 blockId, bytes32 h0, bytes32 h23)
    {
        bytes32 h1 = sha256(abi.encodePacked(l2, l3));
        h0 = bytes32(uint256(0x1234));
        bytes32 h01 = sha256(abi.encodePacked(h0, h1));
        h23 = bytes32(uint256(0x5678));
        blockId = uint256(sha256(abi.encodePacked(h01, h23)));
    }
}
