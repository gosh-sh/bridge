// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeEventVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeEventVerifier.sol";

/// @title AckiNackiBridgeVerifyEventTest
/// @notice Phase A Circuit 4 (Bridge Event Prove) scaffolding tests.
///
/// Drives the `verifyEvent` entry point plus the rolling `_layerWindow` ring
/// buffer using a mock `IBridgeEventVerifier` (the real gnark wrap for
/// Circuit 4 doesn't exist yet — see `docs/circuit_4_open_questions.md`).
/// Covers:
///
/// 1. **Constructor wiring**: enabled vs disabled, missing `dappFr`/`accFr`.
/// 2. **layerWindow plumbing**: every `verifyBlock` pushes the new top-of-chain
///    anchor into one ring-buffer slot; ring buffer wraps cleanly after
///    `LAYER_WINDOW_SIZE` entries; the `LayerWindowPushed` event mirrors the
///    underlying state change.
/// 3. **verifyEvent behaviour**: disabled → revert, mock rejects → revert,
///    mock accepts → emits `BridgeEventVerified`; the bridge forwards the
///    on-chain `_layerWindow` (proved via the strict-mode mock) and the
///    immutable `(dappFr, accFr)` identity pair (also strict-mode).
/// 4. **Phase A trade-off**: `verifyEvent` is intentionally replayable in
///    Phase A — replay protection requires a circuit-side nullifier (Phase B,
///    see Q-CIRC4-2 in `docs/circuit_4_open_questions.md`).
contract AckiNackiBridgeVerifyEventTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeEventVerifier internal bridgeEventVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xC10C40001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal constant LAYER_WINDOW_SIZE = 100;

    event BlockVerified(
        uint256 indexed blockId,
        uint64 indexed blockSeqNo,
        AckiNackiBridge.FinalizationType finType,
        uint8 numLayers
    );

    event LayerWindowPushed(uint256 indexed slot, uint256 anchor, uint256 headAfter);

    event BridgeEventVerified(uint256 indexed tokenId, address indexed submitter);

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        bridgeEventVerifier = new MockBridgeEventVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        bridgeEventVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withBridgeEvent(
                IBridgeEventVerifier(address(bridgeEventVerifier)), DAPP_FR, ACC_FR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    function _layersFor(uint256 blockIdx) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            arr[i] = uint256(keccak256(abi.encode("c4layer", blockIdx, i)));
        }
    }

    function _submitBlock(uint256 blockIdx) internal returns (uint256 topAnchor) {
        uint256[10] memory layers = _layersFor(blockIdx);
        uint256 blockId = FIRST_BLOCK_ID + blockIdx - 1;
        uint64 seqNo = FIRST_SEQ_NO + uint64(blockIdx) - 1;
        topAnchor = layers[ACTIVE_LAYERS - 1];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256(abi.encode("att", blockIdx))),
            abi.encodePacked(keccak256(abi.encode("lh", blockIdx))),
            blockId,
            BK_SET,
            seqNo,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );
    }

    function _submitMany(uint256 n) internal {
        for (uint256 i = 1; i <= n; i++) {
            _submitBlock(i);
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Constructor wiring
    // ─────────────────────────────────────────────────────────────────────

    function test_constructor_bridgeEventVerifierDisabled_acceptsZeroDappAcc() public {
        // Disabled wiring should ignore dapp/acc values entirely.
        AckiNackiBridge plain = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        assertEq(address(plain.bridgeEventVerifier()), address(0));
        assertEq(plain.bridgeEventDappFr(), 0);
        assertEq(plain.bridgeEventAccFr(), 0);
    }

    function test_constructor_bridgeEventEnabledWithZeroDappFr_reverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidBridgeEventIdentity.selector);
        new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.withBridgeEvent(
                IBridgeEventVerifier(address(bridgeEventVerifier)), 0, ACC_FR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_constructor_bridgeEventEnabledWithZeroAccFr_reverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidBridgeEventIdentity.selector);
        new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.withBridgeEvent(
                IBridgeEventVerifier(address(bridgeEventVerifier)), DAPP_FR, 0
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_constructor_bridgeEventEnabled_storesIdentity() public view {
        assertEq(address(bridge.bridgeEventVerifier()), address(bridgeEventVerifier));
        assertEq(bridge.bridgeEventDappFr(), DAPP_FR);
        assertEq(bridge.bridgeEventAccFr(), ACC_FR);
    }

    // ─────────────────────────────────────────────────────────────────────
    // layerWindow plumbing
    // ─────────────────────────────────────────────────────────────────────

    function test_layerWindow_initiallyAllZeroAndHeadZero() public view {
        assertEq(bridge.layerWindowHead(), 0);
        uint256[LAYER_WINDOW_SIZE] memory snap = bridge.getLayerWindow();
        for (uint256 i = 0; i < LAYER_WINDOW_SIZE; i++) {
            assertEq(snap[i], 0);
        }
    }

    function test_verifyBlock_pushesIntoLayerWindow_andEmitsEvent() public {
        // Predict the first push: slot 0, anchor = layers[ACTIVE_LAYERS-1], headAfter = 1.
        uint256[10] memory layers = _layersFor(1);
        uint256 expectedAnchor = layers[ACTIVE_LAYERS - 1];

        vm.expectEmit(true, false, false, true);
        emit LayerWindowPushed(0, expectedAnchor, 1);

        _submitBlock(1);

        assertEq(bridge.layerWindowHead(), 1);
        assertEq(bridge.layerWindowAt(0), expectedAnchor);
        assertEq(bridge.layerWindowAt(1), 0);
        assertTrue(bridge.isLayerHashInWindow(expectedAnchor));
        assertFalse(bridge.isLayerHashInWindow(0xDEAD_BEEF));
    }

    function test_layerWindow_ringBufferWrapsAfterMaxSize() public {
        // Drive 102 blocks. After block 102, head = 102; slots 0 and 1 should
        // hold the anchors of blocks 101 and 102 (the wrap-around overwrites
        // the original blocks 1 and 2); slots 2..99 still hold blocks 3..100.
        _submitMany(102);

        assertEq(bridge.layerWindowHead(), 102);

        uint256 anchor101 = _layersFor(101)[ACTIVE_LAYERS - 1];
        uint256 anchor102 = _layersFor(102)[ACTIVE_LAYERS - 1];
        uint256 anchor3 = _layersFor(3)[ACTIVE_LAYERS - 1];
        uint256 anchor100 = _layersFor(100)[ACTIVE_LAYERS - 1];
        uint256 anchor1 = _layersFor(1)[ACTIVE_LAYERS - 1];

        assertEq(bridge.layerWindowAt(0), anchor101, "slot 0 := block 101");
        assertEq(bridge.layerWindowAt(1), anchor102, "slot 1 := block 102");
        assertEq(bridge.layerWindowAt(2), anchor3, "slot 2 still := block 3");
        assertEq(bridge.layerWindowAt(99), anchor100, "slot 99 still := block 100");

        // Block 1's anchor was overwritten; verify isLayerHashInWindow.
        assertFalse(bridge.isLayerHashInWindow(anchor1), "anchor 1 evicted");
        assertTrue(bridge.isLayerHashInWindow(anchor101));
        assertTrue(bridge.isLayerHashInWindow(anchor102));
    }

    function test_layerWindowAt_outOfBounds_reverts() public {
        vm.expectRevert(bytes("slot oob"));
        bridge.layerWindowAt(LAYER_WINDOW_SIZE);
    }

    function test_getLayerWindow_returnsFullSnapshot() public {
        _submitMany(3);
        uint256[LAYER_WINDOW_SIZE] memory snap = bridge.getLayerWindow();
        for (uint256 i = 0; i < 3; i++) {
            assertEq(snap[i], _layersFor(i + 1)[ACTIVE_LAYERS - 1]);
        }
        // Remaining slots empty.
        for (uint256 i = 3; i < LAYER_WINDOW_SIZE; i++) {
            assertEq(snap[i], 0);
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // verifyEvent behaviour
    // ─────────────────────────────────────────────────────────────────────

    function test_verifyEvent_disabled_reverts() public {
        AckiNackiBridge plain = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        vm.expectRevert(AckiNackiBridge.VerifyEventDisabled.selector);
        plain.verifyEvent(hex"deadbeef", 42);
    }

    function test_verifyEvent_verifierRejects_reverts() public {
        bridgeEventVerifier.setShouldAccept(false);
        vm.expectRevert(AckiNackiBridge.BridgeEventProofRejected.selector);
        bridge.verifyEvent(hex"deadbeef", 42);
    }

    function test_verifyEvent_happyPath_emitsBridgeEventVerified() public {
        _submitBlock(1);

        vm.expectEmit(true, true, false, false);
        emit BridgeEventVerified(7, address(this));

        bool ok = bridge.verifyEvent(hex"cafebabe", 7);
        assertTrue(ok);
    }

    function test_verifyEvent_forwardsBridgeWindowSnapshot() public {
        _submitMany(5);

        // Pin the mock: only return true if the forwarded layerHashes equal
        // the bridge's current window. A passing test ⇒ the bridge did
        // forward its own storage (and not a caller-supplied alternative).
        uint256[LAYER_WINDOW_SIZE] memory snap = bridge.getLayerWindow();
        bridgeEventVerifier.setExpectedLayerHashes(snap);

        bool ok = bridge.verifyEvent(hex"00", 1);
        assertTrue(ok, "bridge should forward its current window verbatim");

        // Sanity: if we mutate the expected window, the mock should now
        // refuse and the bridge should revert (proves the strict check is
        // actually distinguishing values).
        snap[0] = snap[0] ^ 1;
        bridgeEventVerifier.setExpectedLayerHashes(snap);
        vm.expectRevert(AckiNackiBridge.BridgeEventProofRejected.selector);
        bridge.verifyEvent(hex"00", 1);
    }

    function test_verifyEvent_forwardsImmutableIdentityTriple() public {
        _submitBlock(1);

        bridgeEventVerifier.setExpectedIdentity(123, DAPP_FR, ACC_FR);
        bool ok = bridge.verifyEvent(hex"00", 123);
        assertTrue(ok, "tokenId + (dappFr, accFr) forwarded as expected");

        // Wrong tokenId ⇒ mock refuses ⇒ bridge reverts.
        vm.expectRevert(AckiNackiBridge.BridgeEventProofRejected.selector);
        bridge.verifyEvent(hex"00", 124);
    }

    function test_verifyEvent_doesNotMutateLayerWindowOrVerifyBlockState() public {
        _submitMany(3);
        uint256 headBefore = bridge.layerWindowHead();
        uint256 lastSeenBefore = bridge.storedLastSeenBlockSeqNo();
        uint256 anchorBefore = bridge.storedPrevMaxLevelLayerHash();

        bridge.verifyEvent(hex"00", 42);

        assertEq(bridge.layerWindowHead(), headBefore, "head unchanged");
        assertEq(bridge.storedLastSeenBlockSeqNo(), lastSeenBefore, "lastSeen unchanged");
        assertEq(bridge.storedPrevMaxLevelLayerHash(), anchorBefore, "anchor unchanged");
    }

    /// @notice **Phase A trade-off**: Circuit 4 doesn't expose a nullifier yet,
    ///         so `verifyEvent` can be replayed arbitrarily. The bridge does
    ///         NOT prevent this in Phase A — the only consumer today is
    ///         off-chain observation, where idempotent replay is benign.
    ///         When Circuit 4 grows a nullifier (Q-CIRC4-2), the contract
    ///         will gain a nullifier registry and this test will be flipped.
    function test_verifyEvent_isReplayable_byDesign_inPhaseA() public {
        _submitBlock(1);
        assertTrue(bridge.verifyEvent(hex"00", 42));
        assertTrue(bridge.verifyEvent(hex"00", 42));
        assertTrue(bridge.verifyEvent(hex"00", 42));
    }
}
