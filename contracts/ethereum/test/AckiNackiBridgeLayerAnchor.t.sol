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

/// @title AckiNackiBridgeLayerAnchorTest
/// @notice Regression for AB-Q4 — the chain anchor (`prevMaxLevelLayerHash`)
///         must be derived PER LAYER, mirroring the partner prover's
///         `BridgeState::prev_max_level_layer_hash_for`
///         (`bridge-prover-lib/src/bridge_state.rs`):
///
///           t    = highest active layer (active-layer count)
///           pick = min(newNumLayers, t)
///           anchor = latest hash of layer `pick`   (genesis seed when t == 0)
///
///         The bug: `verifyBlock` used a single flat
///         `storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1]` (the
///         previous block's *top* layer). That matches the prover only while
///         `numLayers` is non-decreasing. The moment `numLayers` *decreases*
///         between two consecutive key blocks (e.g. a 3-layer block followed by
///         a 1-layer block, per the `BWS^L` boundary schedule), the prover
///         anchors the shallower block at the previous SAME-LEVEL root while
///         the contract still demanded the previous TOP root — so `verifyBlock`
///         reverted `PrevAnchorMismatch` forever (a liveness halt).
///
///         This suite drives A(numLayers=3) → B(numLayers=1) → C(numLayers=2)
///         with mock verifiers and asserts the per-layer pick is accepted, that
///         higher-layer roots survive a shallower successor, and that the old
///         flat top-layer anchor is now rejected for the shrink step.
contract AckiNackiBridgeLayerAnchorTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;

    // Distinct, easily-recognisable layer-root values.
    uint256 internal constant A1 = 0xA1;
    uint256 internal constant A2 = 0xA2;
    uint256 internal constant A3 = 0xA3;
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

    /// @notice The genesis seed is the anchor for the very first block, then
    ///         the per-layer windows take over.
    function test_firstBlock_usesGenesisSeed() public view {
        assertEq(bridge.expectedPrevAnchor(3), GENESIS_PREV_ANCHOR, "t==0 -> genesis seed");
    }

    /// @notice A(3) -> B(1) -> C(2): the shrink (B) and the regrow (C) both
    ///         anchor at the correct previous SAME-LEVEL root.
    function test_shrinkThenGrow_perLayerAnchor() public {
        // ── Block A: 3 layers. Anchored at the genesis seed (t == 0). ──
        uint256[] memory aActive = new uint256[](3);
        aActive[0] = A1;
        aActive[1] = A2;
        aActive[2] = A3;
        uint256[10] memory layersA = _layers(aActive);

        assertEq(bridge.expectedPrevAnchor(3), GENESIS_PREV_ANCHOR, "A anchor = genesis");
        _submit(0xA, 1, 3, layersA, GENESIS_PREV_ANCHOR);
        assertEq(bridge.storedNumLayers(), 3, "A numLayers");

        // ── Block B: 1 layer (a DECREASE from 3). ──
        // Old (buggy) contract demanded A3 (A's top). The prover anchors B's L1
        // at A's L1 — pick = min(1, t=3) = 1 -> latest of layer 1 == A1.
        assertEq(bridge.expectedPrevAnchor(1), A1, "B anchor = A's L1 (not A3)");

        // The old flat top-layer anchor (A3) must now be REJECTED for B.
        uint256[] memory bActive = new uint256[](1);
        bActive[0] = B1;
        uint256[10] memory layersB = _layers(bActive);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.PrevAnchorMismatch.selector, A3, A1));
        _submit(0xB, 2, 1, layersB, A3);

        // The correct per-layer anchor (A1) is accepted.
        _submit(0xB, 2, 1, layersB, A1);
        assertEq(bridge.storedNumLayers(), 1, "B numLayers");

        // ── Block C: 2 layers (a regrow). ──
        // pick = min(2, t=3) = 2 -> latest of layer 2. A's L2 (A2) must have
        // SURVIVED block B's shallower commit — the per-layer windows are not
        // clobbered by a 1-layer successor.
        assertEq(bridge.expectedPrevAnchor(2), A2, "C anchor = A's L2 (survived B)");

        uint256[] memory cActive = new uint256[](2);
        cActive[0] = C1;
        cActive[1] = C2;
        uint256[10] memory layersC = _layers(cActive);
        _submit(0xC, 3, 2, layersC, A2);
        assertEq(bridge.storedNumLayers(), 2, "C numLayers");

        // Layer-1 window now holds A1, B1, C1 (C is a 2-layer block, so its L1
        // root C1 was appended too); the latest is C1, so a future 1-layer
        // block would anchor at C1.
        assertEq(bridge.expectedPrevAnchor(1), C1, "next L1 anchor = C1");
        // Layer-3 window still holds A3 untouched (never overwritten by B or C).
        assertTrue(bridge.isKnownLayerAnchor(3, A3), "A3 preserved in layer-3 window");
    }

    function _lh(uint256 blockIdx, uint256 layer) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode("L", blockIdx, layer)));
    }

    function _build(uint256 blockIdx, uint8 numLayers)
        internal
        pure
        returns (uint256[10] memory arr)
    {
        for (uint256 i = 0; i < numLayers; i++) {
            arr[i] = _lh(blockIdx, i + 1);
        }
    }

    /// @notice Walk that hits all four of the partner's "General Case B/C"
    ///         transitions in one sequence, asserting the per-layer anchor pick
    ///         `min(numLayers, t)` at every step and that each block is
    ///         accepted. Maps 1:1 onto Alina's diagram:
    ///
    ///           blk numLayers  t(before)  case        pick  anchor
    ///            1      1          0       genesis      -    GENESIS seed
    ///            2      3          1       B  (+2 grow) 1    L1 of blk1
    ///            3      4          3       B  (+1 grow) 3    L3 of blk2
    ///            4      3          4       C  (-1 shrink)3   L3 of blk3
    ///            5      2          4       C  (-2 shrink)2   L2 of blk4
    function test_growShrinkWalk_allFourCases_matchPerLayerPick() public {
        // ── blk1: numLayers=1, anchored at the genesis seed (t == 0). ──
        assertEq(bridge.expectedPrevAnchor(1), GENESIS_PREV_ANCHOR, "blk1 anchor = genesis");
        _submit(0x1, 1, 1, _build(1, 1), GENESIS_PREV_ANCHOR);

        // ── blk2: numLayers=3 — Case B, grow +2 (t was 1). pick=min(3,1)=1. ──
        assertEq(bridge.expectedPrevAnchor(3), _lh(1, 1), "blk2 (B+2) anchor = blk1 L1");
        _submit(0x2, 2, 3, _build(2, 3), _lh(1, 1));

        // ── blk3: numLayers=4 — Case B, grow +1 (t was 3). pick=min(4,3)=3. ──
        assertEq(bridge.expectedPrevAnchor(4), _lh(2, 3), "blk3 (B+1) anchor = blk2 L3");
        _submit(0x3, 3, 4, _build(3, 4), _lh(2, 3));

        // ── blk4: numLayers=3 — Case C, shrink -1 (t=4). pick=min(3,4)=3. ──
        // The old flat top anchor (blk3 L4) must now be rejected.
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.PrevAnchorMismatch.selector, _lh(3, 4), _lh(3, 3)
            )
        );
        _submit(0x4, 4, 3, _build(4, 3), _lh(3, 4));
        assertEq(bridge.expectedPrevAnchor(3), _lh(3, 3), "blk4 (C-1) anchor = blk3 L3");
        _submit(0x4, 4, 3, _build(4, 3), _lh(3, 3));

        // ── blk5: numLayers=2 — Case C, shrink -2 (t still 4). pick=min(2,4)=2. ──
        assertEq(bridge.expectedPrevAnchor(2), _lh(4, 2), "blk5 (C-2) anchor = blk4 L2");
        _submit(0x5, 5, 2, _build(5, 2), _lh(4, 2));

        // Higher layers untouched by the shallow successors survive.
        assertTrue(bridge.isKnownLayerAnchor(4, _lh(3, 4)), "blk3 L4 preserved in layer-4 window");
        assertEq(bridge.storedNumLayers(), 2, "final numLayers = 2");
    }

    /// @notice A non-decreasing sequence keeps the previous behaviour exactly:
    ///         the anchor equals the previous block's top layer.
    function test_nonDecreasing_anchorMatchesPreviousTop() public {
        uint256[] memory aActive = new uint256[](2);
        aActive[0] = A1;
        aActive[1] = A2;
        _submit(0xA, 1, 2, _layers(aActive), GENESIS_PREV_ANCHOR);

        // Same depth (2): pick = min(2, 2) = 2 -> A2 (previous top). Unchanged.
        assertEq(bridge.expectedPrevAnchor(2), A2, "same-depth anchor = prev top");

        // Grow to 3: pick = min(3, 2) = 2 -> A2 (previous top). Unchanged.
        assertEq(bridge.expectedPrevAnchor(3), A2, "grow anchor = prev top");
    }
}
