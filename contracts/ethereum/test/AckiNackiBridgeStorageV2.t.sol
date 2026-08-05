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

/// @title AckiNackiBridgeStorageV2Test
/// @notice Storage v2.0 (2026-08-04) invariants — see `docs/storage_v2_abi_note.md`.
///
/// Coverage:
/// 1. `storedPrevMaxLevelLayerHash()` is IMMUTABLE — always returns the
///    constructor value, no matter how many verifyBlock calls land.
/// 2. `getLatestPerLayer()[L-1]` tracks the last non-zero anchor per layer
///    across a mixed-depth sequence.
/// 3. A shallow-successor-after-deep no longer zeroes tail slots — the
///    key semantic difference from the removed `storedLayerHashes` cache.
contract AckiNackiBridgeStorageV2Test is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10CDEADBEEF;

    // Distinct, easily-recognisable layer-root values.
    uint256 internal constant A1 = 0xA1;
    uint256 internal constant A2 = 0xA2;
    uint256 internal constant A3 = 0xA3;
    uint256 internal constant A4 = 0xA4;
    uint256 internal constant B1 = 0xB1;
    uint256 internal constant C1 = 0xC1;
    uint256 internal constant C2 = 0xC2;

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primaryVerifier)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layerHashesVerifier)),
            BK_SET,
            GENESIS_PREV_ANCHOR
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

    function _layers(uint256[] memory active) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < active.length; i++) {
            arr[i] = active[i];
        }
    }

    function _submit(
        uint256 blockId,
        uint64 seqNo,
        uint8 numLayers,
        uint256[10] memory layerHashes,
        uint256 prevAnchor
    ) internal {
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            blockId,
            BK_SET,
            seqNo,
            numLayers,
            layerHashes,
            prevAnchor
        );
    }

    /// @notice `storedPrevMaxLevelLayerHash()` is now immutable — it MUST
    ///         return the constructor value across the whole verifyBlock
    ///         sequence. This is the core NB-Q3 invariant of storage v2.0.
    function test_storedPrevMaxLevelLayerHash_immutableAcrossVerifyBlocks() public {
        assertEq(bridge.storedPrevMaxLevelLayerHash(), GENESIS_PREV_ANCHOR, "genesis at t=0");

        // Block 1: 3 layers, anchored at genesis.
        uint256[] memory a = new uint256[](3);
        a[0] = A1;
        a[1] = A2;
        a[2] = A3;
        _submit(0xA, 1, 3, _layers(a), GENESIS_PREV_ANCHOR);
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            GENESIS_PREV_ANCHOR,
            "unchanged after block 1"
        );

        // Block 2: 1 layer, anchored at A1 (per-layer pick).
        uint256[] memory b = new uint256[](1);
        b[0] = B1;
        _submit(0xB, 2, 1, _layers(b), A1);
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            GENESIS_PREV_ANCHOR,
            "unchanged after block 2"
        );

        // Block 3: 2 layers, anchored at A2 (per-layer pick).
        uint256[] memory c = new uint256[](2);
        c[0] = C1;
        c[1] = C2;
        _submit(0xC, 3, 2, _layers(c), A2);
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            GENESIS_PREV_ANCHOR,
            "unchanged after block 3"
        );
    }

    /// @notice `getLatestPerLayer()[L-1]` tracks the most-recently-appended
    ///         anchor for layer L. Sequence A(4) -> B(1) -> C(2) drives every
    ///         layer at least once and shows layer-4 survives shallower
    ///         successors.
    function test_getLatestPerLayer_tracksHeadPerLayerAcrossMixedDepths() public {
        // Block A: 4 layers.
        uint256[] memory a = new uint256[](4);
        a[0] = A1;
        a[1] = A2;
        a[2] = A3;
        a[3] = A4;
        _submit(0xA, 1, 4, _layers(a), GENESIS_PREV_ANCHOR);
        uint256[10] memory afterA = bridge.getLatestPerLayer();
        assertEq(afterA[0], A1, "L1 head = A1");
        assertEq(afterA[1], A2, "L2 head = A2");
        assertEq(afterA[2], A3, "L3 head = A3");
        assertEq(afterA[3], A4, "L4 head = A4");
        assertEq(afterA[4], 0, "L5 empty");
        assertEq(afterA[9], 0, "L10 empty");

        // Block B: 1 layer. Only L1's head advances; L2..L4 preserved.
        uint256[] memory b = new uint256[](1);
        b[0] = B1;
        _submit(0xB, 2, 1, _layers(b), A1);
        uint256[10] memory afterB = bridge.getLatestPerLayer();
        assertEq(afterB[0], B1, "L1 head = B1 (advanced)");
        assertEq(afterB[1], A2, "L2 head preserved");
        assertEq(afterB[2], A3, "L3 head preserved");
        assertEq(afterB[3], A4, "L4 head preserved");
        assertEq(afterB[4], 0, "L5 still empty");

        // Block C: 2 layers. L1 and L2 heads advance; L3..L4 preserved.
        uint256[] memory c = new uint256[](2);
        c[0] = C1;
        c[1] = C2;
        _submit(0xC, 3, 2, _layers(c), A2);
        uint256[10] memory afterC = bridge.getLatestPerLayer();
        assertEq(afterC[0], C1, "L1 head = C1");
        assertEq(afterC[1], C2, "L2 head = C2");
        assertEq(afterC[2], A3, "L3 head preserved through C");
        assertEq(afterC[3], A4, "L4 head preserved through C");
        assertEq(afterC[4], 0, "L5 still empty");
    }

    /// @notice The key v2 semantic: a shallow successor no longer clobbers
    ///         deeper slots with zero. Under v1, `storedLayerHashes[3]`
    ///         (0-indexed) would have gone to 0 after block B (numLayers=1)
    ///         because the flat-loop overwrite walked all 10 slots.
    function test_shallowSuccessor_doesNotZeroDeepLayers() public {
        // Deep block: fill 4 layers.
        uint256[] memory a = new uint256[](4);
        a[0] = A1;
        a[1] = A2;
        a[2] = A3;
        a[3] = A4;
        _submit(0xA, 1, 4, _layers(a), GENESIS_PREV_ANCHOR);

        // Shallow successor: 1 layer.
        uint256[] memory b = new uint256[](1);
        b[0] = B1;
        _submit(0xB, 2, 1, _layers(b), A1);

        uint256[10] memory latest = bridge.getLatestPerLayer();
        assertEq(latest[3], A4, "L4 (index 3) NOT zeroed by shallow B");
        // isKnownLayerAnchor confirms the window itself still has the entry.
        assertTrue(bridge.isKnownLayerAnchor(4, A4), "A4 still in layer-4 window");
        // v1 would have set slot 3 to zero here; v2 preserves it.
    }
}
