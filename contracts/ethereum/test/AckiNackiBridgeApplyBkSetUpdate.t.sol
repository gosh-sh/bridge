// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeApplyBkSetUpdateTest
contract AckiNackiBridgeApplyBkSetUpdateTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    uint64 internal constant SEQ = 42;

    event BkSetUpdated(
        uint256 indexed oldCommitment, uint256 indexed newCommitment, uint64 indexed blockSeqNo
    );

    function setUp() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();

        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primary)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layer)),
            L2,
            0
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

    function test_applyBkSetUpdate_happyPath() public {
        (uint256 blockId, bytes32 h0, bytes32 h23) = _merkleWitness(L2, L3);

        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, SEQ);

        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary, hex"00", blockId, SEQ, L2, L3, h0, h23
        );

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), SEQ);
    }

    function test_applyBkSetUpdate_revertsOnMerkleMismatch() public {
        (uint256 blockId,,) = _merkleWitness(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkUpdateMerkleMismatch.selector, blockId, blockId + 1
            )
        );
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId + 1,
            SEQ,
            L2,
            L3,
            bytes32(uint256(0x1234)),
            bytes32(uint256(0x5678))
        );
    }

    function test_applyBkSetUpdate_revertsWhenAttestationRejected() public {
        primary.setShouldAccept(false);
        (uint256 blockId, bytes32 h0, bytes32 h23) = _merkleWitness(L2, L3);

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary, hex"00", blockId, SEQ, L2, L3, h0, h23
        );
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
