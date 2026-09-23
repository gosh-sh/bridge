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

/// @title AckiNackiBridgeApplyBkSetUpdateOrderingTest
/// @notice After `applyBkSetUpdate(N)`, `verifyBlock` accepts the previous
///         BK-set for `blockSeqNo <= N`. The first rotation may land
///         before the layer cursor covers N (off-boundary rotations have
///         no bundle at N). A second rotation is blocked until
///         `storedLastSeenBlockSeqNo >=` the previous N — only one
///         outgoing set is stored.
///
/// @dev The pre-existing `AckiNackiBridgeApplyBkSetUpdate.t.sol` covers the
///      other rotation invariants (canonicity, Merkle fold, monotonicity, ...).
///      This suite is scoped strictly to the ordering hazard.
contract AckiNackiBridgeApplyBkSetUpdateOrderingTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;
    MockFallbackVerifier internal fallbackVerifier;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    /// @dev Rotation seq_no. Small so `_primeLayerCursor` fast-forwards
    ///      with a single `verifyBlock` in tests that need one.
    uint64 internal constant N = 5;

    /// @dev BN254 scalar field order.
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    bytes32 internal constant SIB_H01 = bytes32(uint256(0x1234));
    bytes32 internal constant SIB_H4_7 = bytes32(uint256(0x5678));
    bytes32 internal constant SIB_H8_15 = bytes32(uint256(0x9ABC));

    event BkSetUpdated(
        uint256 indexed oldCommitment, uint256 indexed newCommitment, uint64 indexed blockSeqNo
    );

    function setUp() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primary = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();

        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layer.setShouldAccept(true);

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

    // -----------------------------------------------------------------
    // Case 1 — direct rotation ahead of a fresh layer cursor
    // -----------------------------------------------------------------

    /// @notice Fresh deploy, `storedLastSeenBlockSeqNo == 0`. The first
    ///         rotation may apply; `verifyBlock(N)` then still binds to OLD.
    function test_applyBkSetUpdate_aheadOfLayerCursor_primary_thenVerifyOld() public {
        _assertPristine();
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedPrevBkSetCommitment(), L2);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
        assertEq(bridge.storedLastSeenBlockSeqNo(), 0, "rotation must not advance the layer cursor");

        _primeLayerCursor(N, L2);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
    }

    /// @notice Symmetric on the Fallback path.
    function test_applyBkSetUpdate_aheadOfLayerCursor_fallback_thenVerifyOld() public {
        _assertPristine();
        _applyFallback(_merkleRoot(L2, L3), N, L2, L3);
        _primeLayerCursor(N, L2);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
        assertEq(bridge.storedBkSetCommitment(), L3);
    }

    // -----------------------------------------------------------------
    // Case 2 — equality boundary (`blockSeqNo == storedLastSeenBlockSeqNo`)
    // -----------------------------------------------------------------

    /// @notice The invariant is `<=`, not `<`. The combined-block correct-order
    ///         path lands here: `verifyBlock(N)` sets `storedLastSeen == N`,
    ///         then `applyBkSetUpdate(N)` must proceed.
    function test_applyBkSetUpdate_atLayerCursor_succeeds() public {
        _primeLayerCursor(N, L2);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N, "layer cursor at N");

        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, N);
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3, "commitment rotated");
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N, "update cursor advanced");
        assertEq(bridge.storedLastSeenBlockSeqNo(), N, "rotation must not advance the layer cursor");
    }

    // -----------------------------------------------------------------
    // Case 3 — combined block (rotation + bundle boundary at same block N)
    // -----------------------------------------------------------------

    /// @notice Correct order: `verifyBlock(N)` first, then `applyBkSetUpdate(N)`.
    ///         Both succeed; both cursors advance to `N`.
    function test_applyBkSetUpdate_combinedBlock_correctOrder_succeeds() public {
        _primeLayerCursor(N, L2);

        uint256 root = _merkleRoot(L2, L3);
        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, N);
        _applyPrimary(root, N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedPrevBkSetCommitment(), L2);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
    }

    /// @notice Apply first, then `verifyBlock(N)` under OLD. The rotation
    ///         block is signed by the outgoing set; accepting the previous
    ///         commitment is what makes apply-ahead safe.
    function test_applyBkSetUpdate_thenVerifyBlock_acceptsPrevCommitment() public {
        _assertPristine();
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);
        _primeLayerCursor(N, L2);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedPrevBkSetCommitment(), L2);
    }

    // -----------------------------------------------------------------
    // Case 4 — adversarial spam
    // -----------------------------------------------------------------

    /// @notice A second rotation while the layer cursor is still behind the
    ///         first N reverts and leaves the first rotation in place.
    function test_applyBkSetUpdate_secondRotation_revertsUntilPreviousCovered() public {
        uint256 root = _merkleRoot(L2, L3);
        _applyPrimary(root, N, L2, L3);

        uint256 l4 = 0xC0DE;
        uint256 root2 = _merkleRoot(L3, l4);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, N, uint64(0)
            )
        );
        _applyPrimary(root2, N + 3, L3, l4);

        assertEq(bridge.storedBkSetCommitment(), L3, "first rotation kept");
        assertEq(bridge.storedPrevBkSetCommitment(), L2);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);

        _primeLayerCursor(N, L2);
        _applyPrimary(root2, N + 3, L3, l4);
        assertEq(bridge.storedBkSetCommitment(), l4);
        assertEq(bridge.storedPrevBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N + 3);
    }

    /// @notice Lower edge of the second-rotation guard: cursor = N1 − 1
    ///         still reverts, and the first field is the previous N.
    function test_applyBkSetUpdate_secondRotation_revertsAtCursorN1MinusOne() public {
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);
        _primeLayerCursor(N - 1, L2);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N - 1);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);

        uint256 l4 = 0xC0DE;
        uint256 root2 = _merkleRoot(L3, l4);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, uint64(N), uint64(N - 1)
            )
        );
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            root2,
            N + 3,
            0,
            L3,
            l4,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    /// @notice Fallback path forwards a non-zero baked `lastSeen`.
    function test_applyBkSetUpdate_fallback_forwardsBakedLastSeen() public {
        fallbackVerifier.setExpectedLastSeenBlockSeqNo(3);
        _applyFallbackWithLastSeen(_merkleRoot(L2, L3), N, 3, L2, L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
    }

    /// @notice After the cursor covers the first rotation, a further
    ///         rotation may apply even if it is ahead of the cursor.
    function test_applyBkSetUpdate_secondRotation_afterCover_mayLeadCursor() public {
        _primeLayerCursor(N, L2);
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);

        uint256 l4 = 0xC0DE;
        _applyPrimary(_merkleRoot(L3, l4), N + 3, L3, l4);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N + 3);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
        _primeLayerCursor(N + 1, L3);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N + 1);
    }

    // -----------------------------------------------------------------
    // Case 5 — `_expectedBkSetFor` negatives
    // -----------------------------------------------------------------

    /// @notice After apply(N) a later block signed by the outgoing set
    ///         reverts. This is the point of rotating.
    function test_verifyBlock_afterRotation_rejectsPrevCommitmentPastN() public {
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);
        uint256 anchor = bridge.expectedPrevAnchor(1);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkSetCommitmentMismatch.selector, L2, L3)
        );
        _verify(N + 1, L2, anchor);
    }

    /// @notice At or below N the new set is not yet the signer.
    function test_verifyBlock_afterRotation_rejectsNewCommitmentAtOrBeforeN() public {
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);
        uint256 anchor = bridge.expectedPrevAnchor(1);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkSetCommitmentMismatch.selector, L3, L2)
        );
        _verify(N, L3, anchor);
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /// @dev Assert every rotation-side state field is at construction defaults
    ///      and the layer cursor has not moved. Used before/after a call that
    ///      must not mutate state.
    function _assertPristine() internal view {
        assertEq(bridge.storedBkSetCommitment(), L2, "BK commitment unchanged");
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), 0, "BK-update cursor unchanged");
        assertEq(bridge.storedLastSeenBlockSeqNo(), 0, "layer cursor unchanged");
    }

    /// @dev Fast-forward `storedLastSeenBlockSeqNo` to `target` via a single
    ///      `verifyBlock` submission. `verifyBlock`'s only monotonicity
    ///      requirement is strict-greater, so any positive `target` works from
    ///      a fresh state.
    function _primeLayerCursor(uint64 target, uint256 bkSet) internal {
        _verify(target, bkSet, bridge.expectedPrevAnchor(1));
    }

    function _verify(uint64 target, uint256 bkSet, uint256 prevAnchor) internal {
        uint256[10] memory layers;
        layers[0] = 1;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            1,
            bkSet,
            target,
            1,
            layers,
            prevAnchor
        );
    }

    function _applyPrimary(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3) internal {
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            seqNo,
            0,
            oldL2,
            newL3,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    function _applyFallback(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3) internal {
        _applyFallbackWithLastSeen(blockId, seqNo, 0, oldL2, newL3);
    }

    function _applyFallbackWithLastSeen(
        uint256 blockId,
        uint64 seqNo,
        uint64 lastSeen,
        uint256 oldL2,
        uint256 newL3
    ) internal {
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Fallback,
            hex"00",
            blockId,
            seqNo,
            lastSeen,
            oldL2,
            newL3,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    /// @dev Depth-4 fold of the L2/L3 pair up to the block-id root, mapped into
    ///      BN254 `Fr`. Mirrors the helper in
    ///      `AckiNackiBridgeApplyBkSetUpdate.t.sol`.
    function _merkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        bytes32 h23 = sha256(abi.encodePacked(_le(l2), _le(l3)));
        bytes32 h0_3 = sha256(abi.encodePacked(SIB_H01, h23));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, SIB_H4_7));
        return uint256(sha256(abi.encodePacked(h0_7, SIB_H8_15))) % R;
    }

    /// @dev Little-endian `Fr::to_repr()` image, matching the bytes the AN
    ///      node hashes into leaves L2/L3.
    function _le(uint256 value) internal pure returns (bytes32) {
        uint256 reversed;
        for (uint256 i = 0; i < 32; i++) {
            reversed = (reversed << 8) | (value & 0xff);
            value >>= 8;
        }
        return bytes32(reversed);
    }
}
