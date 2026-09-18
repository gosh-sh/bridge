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
/// @notice BRIDGE-ETH-WD-2 — `applyBkSetUpdate(N)` must not proceed unless
///         `verifyBlock` has already covered block `N`
///         (`blockSeqNo <= storedLastSeenBlockSeqNo`). Without this invariant,
///         a permissionless caller — buggy relayer, hostile actor
///         front-running the honest relayer at a bundle-boundary rotation —
///         can flip `storedBkSetCommitment` OLD → NEW while block `N`'s
///         attestation is still signed by OLD keys, permanently bricking
///         `verifyBlock(N)` and stranding every future block anchored through
///         `N`'s layers. No admin recovery exists (immutable verifiers, no
///         pause, no state-reset), so the guard has to live on-chain.
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

    /// @notice Fresh deploy, `storedLastSeenBlockSeqNo == 0`. Any rotation
    ///         with `blockSeqNo > 0` must revert `VerifyBlockLagBehindRotation`
    ///         and leave every rotation-side state field untouched.
    function test_applyBkSetUpdate_revertsWhenAheadOfLayerCursor_primary() public {
        _assertPristine();

        // Hoist the SHA-256 precompile fold out of the `vm.expectRevert`
        // window — otherwise the first precompile staticcall from
        // `_merkleRoot` is what `expectRevert` observes, and it returns
        // successfully, so the assertion fails before `applyBkSetUpdate` runs.
        uint256 root = _merkleRoot(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, N, uint64(0)
            )
        );
        _applyPrimary(root, N, L2, L3);

        _assertPristine();
    }

    /// @notice Symmetric on the Fallback path.
    function test_applyBkSetUpdate_revertsWhenAheadOfLayerCursor_fallback() public {
        _assertPristine();

        uint256 root = _merkleRoot(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, N, uint64(0)
            )
        );
        _applyFallback(root, N, L2, L3);

        _assertPristine();
    }

    // -----------------------------------------------------------------
    // Case 2 — equality boundary (`blockSeqNo == storedLastSeenBlockSeqNo`)
    // -----------------------------------------------------------------

    /// @notice The invariant is `<=`, not `<`. The combined-block correct-order
    ///         path lands here: `verifyBlock(N)` sets `storedLastSeen == N`,
    ///         then `applyBkSetUpdate(N)` must proceed.
    function test_applyBkSetUpdate_atLayerCursor_succeeds() public {
        _primeLayerCursor(N);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N, "layer cursor at N");

        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, N);
        _applyPrimary(_merkleRoot(L2, L3), N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3, "commitment rotated");
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N, "update cursor advanced");
        assertEq(
            bridge.storedLastSeenBlockSeqNo(),
            N,
            "rotation must not advance the layer cursor"
        );
    }

    // -----------------------------------------------------------------
    // Case 3 — combined block (rotation + bundle boundary at same block N)
    // -----------------------------------------------------------------

    /// @notice Correct order: `verifyBlock(N)` first, then `applyBkSetUpdate(N)`.
    ///         Both succeed; both cursors advance to `N`.
    function test_applyBkSetUpdate_combinedBlock_correctOrder_succeeds() public {
        _primeLayerCursor(N);

        uint256 root = _merkleRoot(L2, L3);
        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, N);
        _applyPrimary(root, N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
    }

    /// @notice Wrong order: `applyBkSetUpdate(N)` first, on a fresh layer
    ///         cursor. Must revert `VerifyBlockLagBehindRotation`, leave every
    ///         rotation-side field untouched, and let the caller recover with
    ///         `verifyBlock(N)` then `applyBkSetUpdate(N)`. This is the
    ///         "state-invariant under adversarial ordering" property.
    function test_applyBkSetUpdate_combinedBlock_wrongOrder_stateInvariant() public {
        _assertPristine();

        uint256 root = _merkleRoot(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, N, uint64(0)
            )
        );
        _applyPrimary(root, N, L2, L3);

        // No state change from the reverted rotation.
        _assertPristine();

        // Recovery: verifyBlock(N) under OLD succeeds — the guard did not
        // touch `storedBkSetCommitment`, so the attestation still binds to
        // OLD as it must.
        _primeLayerCursor(N);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
        assertEq(
            bridge.storedBkSetCommitment(),
            L2,
            "OLD commitment survived the reverted rotation"
        );

        // Now the rotation goes through.
        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, N);
        _applyPrimary(root, N, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
    }

    // -----------------------------------------------------------------
    // Case 4 — adversarial spam
    // -----------------------------------------------------------------

    /// @notice A hostile actor spamming rotations ahead of the layer cursor
    ///         must not brick anything. Every call reverts, storage stays
    ///         pristine, and an honest `verifyBlock` → `applyBkSetUpdate`
    ///         still succeeds afterwards.
    function test_applyBkSetUpdate_spamAheadOfLayer_allRevert() public {
        uint256 rootFuture = _merkleRoot(L2, L3);

        for (uint64 i = 1; i <= 32; i++) {
            vm.expectRevert(
                abi.encodeWithSelector(
                    AckiNackiBridge.VerifyBlockLagBehindRotation.selector, i, uint64(0)
                )
            );
            _applyPrimary(rootFuture, i, L2, L3);
        }
        _assertPristine();

        // Honest recovery still works — state was never mutated.
        _primeLayerCursor(N);
        _applyPrimary(rootFuture, N, L2, L3);
        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), N);
    }

    /// @notice Adversarial spam with a rotation `blockSeqNo` strictly ahead of
    ///         a partially-advanced layer cursor. The guard fires on every
    ///         call and reports the *current* cursor value in the error data,
    ///         not a stale one.
    function test_applyBkSetUpdate_spamStrictlyAheadOfLayer_reportsLiveCursor() public {
        _primeLayerCursor(N);

        uint256 root = _merkleRoot(L2, L3);
        uint64 target = N + 3;
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.VerifyBlockLagBehindRotation.selector, target, N
            )
        );
        _applyPrimary(root, target, L2, L3);

        // Storage untouched.
        assertEq(bridge.storedBkSetCommitment(), L2);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), 0);
        assertEq(bridge.storedLastSeenBlockSeqNo(), N);
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
    function _primeLayerCursor(uint64 target) internal {
        uint256[10] memory layers;
        layers[0] = 1;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            hex"00",
            1,
            L2,
            target,
            1,
            layers,
            bridge.expectedPrevAnchor(1)
        );
    }

    function _applyPrimary(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3)
        internal
    {
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

    function _applyFallback(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3)
        internal
    {
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Fallback,
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
