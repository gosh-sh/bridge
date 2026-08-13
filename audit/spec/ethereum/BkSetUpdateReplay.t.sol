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
/// @notice Phase C / A2 — A2-INV-6 / BK-5: BK-set update replay rejected (16-leaf tree).
contract BkSetUpdateReplayTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    uint64 internal constant SEQ = 42;

    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    bytes32 internal constant SIB_H01 = bytes32(uint256(0x1234));
    bytes32 internal constant SIB_H4_7 = bytes32(uint256(0x5678));
    bytes32 internal constant SIB_H8_15 = bytes32(uint256(0x9ABC));

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
        _apply(_merkleRoot(L2, L3), SEQ, L2, L3);

        uint256 l4 = 0xC0FFEE;
        uint256 secondBlockId = _merkleRoot(L3, l4);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkUpdateSeqNoNotMonotonic.selector, SEQ, SEQ)
        );
        _apply(secondBlockId, SEQ, L3, l4);
    }

    function _apply(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3) internal {
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            seqNo,
            oldL2,
            newL3,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    function _merkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        return _rawMerkleRoot(l2, l3) % R;
    }

    function _rawMerkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        bytes32 h23 = sha256(abi.encodePacked(_le(l2), _le(l3)));
        bytes32 h0_3 = sha256(abi.encodePacked(SIB_H01, h23));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, SIB_H4_7));
        return uint256(sha256(abi.encodePacked(h0_7, SIB_H8_15)));
    }

    function _le(uint256 value) internal pure returns (bytes32) {
        uint256 reversed;
        for (uint256 i = 0; i < 32; i++) {
            reversed = (reversed << 8) | (value & 0xff);
            value >>= 8;
        }
        return bytes32(reversed);
    }
}
