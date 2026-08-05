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

/// @title AckiNackiBridgeRelayerLoopTest
/// @notice Phase 5.1 — On-chain side of the relayer-loop acceptance.
///
/// Drives `AckiNackiBridge.verifyBlock` through 10 sequential synthetic blocks
/// using mock verifiers, mirroring exactly what the off-chain relayer skeleton
/// (`crates/bridge-relayer-daemon`) produces. The mocks short-circuit the ZK
/// verification step (Circuits 1A/1B/2 are exercised end-to-end with real
/// proofs in `AckiNackiBridgeVerifyBlock.t.sol`); this suite isolates the
/// state-machine invariants that matter for the relayer:
///
/// - `storedLastSeenBlockSeqNo` advances monotonically and **exactly** by one
///   per accepted block (so the relayer's `target = last_seen + 1` query is
///   reliable);
/// - `getLatestPerLayer()` reflects the per-layer head across the accepted
///   blocks (storage v2.0 replacement for the removed `storedNumLayers` +
///   `storedLayerHashes[..]` flat cache);
/// - `expectedPrevAnchor(numLayers)` returns the correct chain anchor to
///   thread into the next block's `prevMaxLevelLayerHash` argument (per-layer
///   pick — see AB-Q4 and `docs/storage_v2_abi_note.md`);
/// - mixing Primary and Fallback finalization types in the same loop works;
/// - one `BlockVerified(blockId, blockSeqNo, finType, numLayers)` event fires
///   per block (no duplicates, no holes).
contract AckiNackiBridgeRelayerLoopTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C; // genesis chain anchor
    uint8 internal constant ACTIVE_LAYERS = 5;
    uint256 internal constant FIRST_BLOCK_ID = 0xB10C0001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    event BlockVerified(
        uint256 indexed blockId,
        uint64 indexed blockSeqNo,
        AckiNackiBridge.FinalizationType finType,
        uint8 numLayers
    );

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

    // ────────────────────────────────────────────────────────────────────
    // Helpers
    // ────────────────────────────────────────────────────────────────────

    /// @dev Synthesises a layer-hash array deterministically from `blockIdx`.
    ///      The chain anchor convention used by Circuit 2 (and enforced by
    ///      `verifyBlock` via the `prevMaxLevelLayerHash` argument) is that
    ///      slot `numLayers - 1` of block N becomes
    ///      `prevMaxLevelLayerHash` of block N+1.
    function _layersFor(uint256 blockIdx) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            arr[i] = uint256(keccak256(abi.encode("layer", blockIdx, i)));
        }
    }

    function _proofBytes(uint256 blockIdx, string memory tag) internal pure returns (bytes memory) {
        // The mock verifiers ignore proof bytes entirely; we still want them
        // distinct so any future signature collision in instrumentation is
        // visible.
        return abi.encodePacked(keccak256(abi.encode("proof", blockIdx, tag)));
    }

    /// @dev Pulls the current chain anchor that the next block must thread in
    ///      as `prevMaxLevelLayerHash`. Storage v2.0 (2026-08-04): sourced
    ///      from `expectedPrevAnchor(numLayers)`, which does the per-layer
    ///      pick `min(numLayers, highestActiveLayer)` off `_layerWindows`.
    ///      All blocks in this suite use `ACTIVE_LAYERS`, so the per-layer
    ///      pick equals `ACTIVE_LAYERS`.
    function _currentAnchor() internal view returns (uint256) {
        return bridge.expectedPrevAnchor(ACTIVE_LAYERS);
    }

    function _submit(uint256 blockIdx, AckiNackiBridge.FinalizationType finType)
        internal
        returns (uint256[10] memory layers)
    {
        layers = _layersFor(blockIdx);
        uint256 blockId = FIRST_BLOCK_ID + blockIdx - 1;
        uint64 seqNo = FIRST_SEQ_NO + uint64(blockIdx) - 1;

        vm.expectEmit(true, true, false, true);
        emit BlockVerified(blockId, seqNo, finType, ACTIVE_LAYERS);

        bridge.verifyBlock(
            finType,
            _proofBytes(blockIdx, "att"),
            _proofBytes(blockIdx, "lh"),
            blockId,
            BK_SET,
            seqNo,
            ACTIVE_LAYERS,
            layers,
            _currentAnchor()
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Acceptance: 10-block loop, mixed Primary/Fallback
    // ────────────────────────────────────────────────────────────────────

    /// @notice Drives 10 sequential blocks (mix of Primary and Fallback) and
    ///         asserts the bridge's exposed AN state matches the latest block
    ///         after every step. This is the on-chain acceptance criterion
    ///         from §5 of `docs/an_partner_integration_plan.md` (relayer
    ///         drives 10 blocks → `storedLastSeenBlockSeqNo` advances by
    ///         exactly 10 → all 10 emit `BlockVerified`).
    function test_relayerLoop_10Blocks_mixedFinTypes_advancesState() public {
        for (uint256 idx = 1; idx <= 10; idx++) {
            AckiNackiBridge.FinalizationType ft = (idx % 3 == 0)
                ? AckiNackiBridge.FinalizationType.Fallback
                : AckiNackiBridge.FinalizationType.Primary;

            uint256[10] memory layers = _submit(idx, ft);

            assertEq(bridge.storedLastSeenBlockSeqNo(), uint64(idx), "seqNo");
            // Storage v2.0: per-layer state observed via `getLatestPerLayer()`.
            // Each layer's head is the just-submitted block's layer hash;
            // slots >= ACTIVE_LAYERS are empty windows -> zero.
            uint256[10] memory latest = bridge.getLatestPerLayer();
            for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
                assertEq(latest[i], layers[i], "layer head");
            }
            for (uint256 i = ACTIVE_LAYERS; i < 10; i++) {
                assertEq(latest[i], 0, "unused-layer stays zero");
            }
            // The next block's expected anchor mirrors the just-committed
            // top-layer head (constant-numLayers stream: pick = ACTIVE_LAYERS).
            assertEq(
                bridge.expectedPrevAnchor(ACTIVE_LAYERS),
                layers[ACTIVE_LAYERS - 1],
                "next-anchor follows last layer"
            );
        }
    }

    /// @notice After 5 blocks, simulate a relayer crash and a "restart": no
    ///         on-chain rewind; the next block (`seqNo = 6`) submits cleanly
    ///         with the live anchor. This proves the relayer's recovery
    ///         strategy ("read on-chain anchor on startup") is sufficient.
    function test_relayerLoop_restartReadsCurrentAnchor() public {
        for (uint256 idx = 1; idx <= 5; idx++) {
            _submit(idx, AckiNackiBridge.FinalizationType.Primary);
        }
        uint256 anchorAfter5 = bridge.expectedPrevAnchor(ACTIVE_LAYERS);

        // "Restart": create a fresh test caller; the bridge state survives.
        address relayerB = address(0xBEEF);
        vm.prank(relayerB);

        uint256[10] memory layers6 = _layersFor(6);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            _proofBytes(6, "att"),
            _proofBytes(6, "lh"),
            FIRST_BLOCK_ID + 5,
            BK_SET,
            6,
            ACTIVE_LAYERS,
            layers6,
            anchorAfter5
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), 6, "seqNo after restart");
        assertEq(
            bridge.expectedPrevAnchor(ACTIVE_LAYERS),
            layers6[ACTIVE_LAYERS - 1],
            "next-anchor advanced after restart"
        );
    }

    /// @notice Replaying the same seqNo reverts with `BlockSeqNoNotMonotonic`.
    ///         Encodes the relayer's idempotency requirement: a crash + retry
    ///         after a successful inclusion must not double-count.
    function test_relayerLoop_replaySameSeqNo_reverts() public {
        _submit(1, AckiNackiBridge.FinalizationType.Primary);
        _submit(2, AckiNackiBridge.FinalizationType.Primary);

        uint256[10] memory layers2 = _layersFor(2);
        // Capture the anchor *before* `vm.expectRevert` — otherwise the
        // `bridge.storedPrevMaxLevelLayerHash()` view call would itself consume
        // the expectRevert directive (Foundry applies it to the very next
        // external call, including views).
        uint256 anchor = _currentAnchor();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BlockSeqNoNotMonotonic.selector, uint64(2), uint64(2)
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            _proofBytes(2, "att"),
            _proofBytes(2, "lh"),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            2,
            ACTIVE_LAYERS,
            layers2,
            anchor
        );
    }

    /// @notice The contract *permits* a strictly-greater seqNo gap (fast-
    ///         forward), so the relayer must enforce `target = last_seen + 1`
    ///         off-chain itself. We assert the on-chain behavior so the
    ///         relayer's policy choice is intentional, not accidental.
    function test_relayerLoop_seqNoFastForward_isPermittedByContract() public {
        _submit(1, AckiNackiBridge.FinalizationType.Primary);
        _submit(2, AckiNackiBridge.FinalizationType.Primary);

        // Skip to seqNo=5; bridge accepts because seqNo > storedLastSeenBlockSeqNo.
        // The relayer never does this in practice (it wants every block).
        uint256[10] memory layers5 = _layersFor(5);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            _proofBytes(5, "att"),
            _proofBytes(5, "lh"),
            FIRST_BLOCK_ID + 4,
            BK_SET,
            5,
            ACTIVE_LAYERS,
            layers5,
            _currentAnchor()
        );
        assertEq(bridge.storedLastSeenBlockSeqNo(), 5);
    }

    /// @notice If the off-chain prover rejects a block (mock returns false),
    ///         `verifyBlock` reverts with `AttestationProofRejected` — and
    ///         on-chain state is untouched. The relayer is responsible for
    ///         retrying or skipping.
    function test_relayerLoop_onAttestationReject_stateUntouched() public {
        _submit(1, AckiNackiBridge.FinalizationType.Primary);
        uint256 anchor = bridge.expectedPrevAnchor(ACTIVE_LAYERS);

        primaryVerifier.setShouldAccept(false);
        uint256[10] memory layers2 = _layersFor(2);
        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            _proofBytes(2, "att"),
            _proofBytes(2, "lh"),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            2,
            ACTIVE_LAYERS,
            layers2,
            anchor
        );
        assertEq(bridge.storedLastSeenBlockSeqNo(), 1, "seqNo unchanged");
        assertEq(bridge.expectedPrevAnchor(ACTIVE_LAYERS), anchor, "next-anchor unchanged");

        primaryVerifier.setShouldAccept(true);
        _submit(2, AckiNackiBridge.FinalizationType.Primary);
        assertEq(bridge.storedLastSeenBlockSeqNo(), 2, "seqNo advances after retry");
    }

    /// @notice Submitting a block whose `prevMaxLevelLayerHash` doesn't match
    ///         the current chain anchor must revert. This is the exact
    ///         scenario the relayer needs to detect when reorg/forks happen
    ///         off-chain.
    function test_relayerLoop_anchorMismatch_reverts() public {
        _submit(1, AckiNackiBridge.FinalizationType.Primary);
        uint256[10] memory layers2 = _layersFor(2);
        uint256 wrongAnchor = uint256(keccak256("wrong-anchor"));
        uint256 storedAnchor = bridge.expectedPrevAnchor(ACTIVE_LAYERS);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.PrevAnchorMismatch.selector, wrongAnchor, storedAnchor
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            _proofBytes(2, "att"),
            _proofBytes(2, "lh"),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            2,
            ACTIVE_LAYERS,
            layers2,
            wrongAnchor
        );
    }
}

