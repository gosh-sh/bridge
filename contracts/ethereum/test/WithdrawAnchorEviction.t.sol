// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalFinalVerifier.sol";
import "../src/IBridgeMultiHopVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalFinalVerifier.sol";
import "./mocks/MockBridgeMultiHopVerifier.sol";
import "./mocks/MockERC20.sol";

/// @title WithdrawAnchorEvictionTest
/// @notice Phase C / A3 — WD-Q1 / WD-6: L1 anchor evicted after 128 newer verifyBlocks.
/// @dev INV: WD-6 — rolling window HISTORY_PROOF_WINDOW = 128
///
///      The withdrawal entry-point this suite exercises is
///      `withdrawByProofBundle` — same-thread claims only, so the hop-chain
///      is always empty and `xBlockId == yBlockId`.
contract WithdrawAnchorEvictionTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalFinalVerifier internal finalVerifier;
    MockBridgeMultiHopVerifier internal multiHopVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xE0100001;
    uint64 internal constant FIRST_SEQ_NO = 1;
    uint256 internal constant WINDOW = 128;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    /// @dev Same-thread claims use a fixed block-id for the FinalProof's X/Y.
    uint256 internal constant SAME_THREAD_BLOCK = 0xB10C41D;

    address internal funder = address(0xF00D);
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        finalVerifier = new MockBridgeWithdrawalFinalVerifier();
        multiHopVerifier = new MockBridgeMultiHopVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        finalVerifier.setShouldAccept(true);
        multiHopVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdrawBundle(
                IBridgeWithdrawalFinalVerifier(address(finalVerifier)),
                IBridgeMultiHopVerifier(address(multiHopVerifier)),
                DAPP_FR,
                ACC_FR
            )
        );
    }

    function _layers(uint256 blockIdx) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            arr[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("evict-layer", blockIdx, i))));
        }
    }

    function _submitBlock(uint256 blockIdx) internal returns (uint256 l1Anchor) {
        uint256[10] memory layers = _layers(blockIdx);
        l1Anchor = layers[0];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256(abi.encode("att", blockIdx))),
            abi.encodePacked(keccak256(abi.encode("lh", blockIdx))),
            FIRST_BLOCK_ID + blockIdx - 1,
            BK_SET,
            FIRST_SEQ_NO + uint64(blockIdx) - 1,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    function _emptyHops()
        internal
        pure
        returns (uint256[][] memory hopPubs, bytes[] memory hopProofs)
    {
        hopPubs = new uint256[][](0);
        hopProofs = new bytes[](0);
    }

    /// @dev QC: WD-Q1 — stale finalRoot becomes UnknownAnchor after window eviction.
    function test_withdrawByProofBundle_evictedAnchor_reverts() public {
        uint256 evictedL1 = _submitBlock(1);
        assertTrue(bridge.isKnownLayerAnchor(1, evictedL1), "pre: first anchor known");

        for (uint256 i = 2; i <= WINDOW; i++) {
            _submitBlock(i);
        }
        assertTrue(bridge.isKnownLayerAnchor(1, evictedL1), "pre: anchor still in full window");

        _submitBlock(WINDOW + 1);
        assertFalse(bridge.isKnownLayerAnchor(1, evictedL1), "post: oldest L1 evicted");
        assertEq(bridge.layerWindowLen(1), WINDOW, "ring stays full after wrap");
        assertEq(bridge.anchorRemainingAppends(1, evictedL1), 0, "evicted is 0");

        uint256[] memory pub = _withdrawPub(evictedL1, 0xDEAD, 1);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnknownAnchor.selector, evictedL1));
        bridge.withdrawByProofBundle(pub, hex"00", hopPubs, hopProofs);
    }

    /// @dev A `blockSeqNo` jump writes one window slot. The earlier L1 hash
    ///      stays known — unbounded seq_no is catch-up, not eviction.
    function test_seqNoFastForward_doesNotEvictEarlierAnchor() public {
        uint256 firstL1 = _submitBlock(1);
        assertTrue(bridge.isKnownLayerAnchor(1, firstL1));

        uint256[10] memory jumped = _layers(2);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("att-jump")),
            abi.encodePacked(keccak256("lh-jump")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            1_000_000,
            ACTIVE_LAYERS,
            jumped,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), 1_000_000);
        assertTrue(
            bridge.isKnownLayerAnchor(1, firstL1), "seq_no jump must not evict the previous L1 hash"
        );
        assertTrue(bridge.isKnownLayerAnchor(1, jumped[0]), "jumped head recorded");
    }

    /// @dev Remaining appends, not occupancy, is the SLA signal.
    function test_eth18_anchorRemainingAppends_countsUntilEviction() public {
        uint256 first = _submitBlock(1);
        assertEq(bridge.layerWindowLen(1), 1);
        assertEq(bridge.anchorRemainingAppends(1, first), WINDOW, "first append survives 128 more");

        for (uint256 i = 2; i <= WINDOW; i++) {
            _submitBlock(i);
        }
        assertEq(bridge.layerWindowLen(1), WINDOW, "occupancy saturated");
        assertEq(bridge.anchorRemainingAppends(1, first), 1, "next append evicts the oldest");

        _submitBlock(WINDOW + 1);
        assertEq(bridge.anchorRemainingAppends(1, first), 0);
        assertEq(bridge.layerWindowLen(1), WINDOW, "occupancy still 128 after wrap");
    }

    /// @dev After the original `finalRoot` is evicted, a new Circuit 4 proof
    ///      bound to a still-in-window descendant must pay.
    ///      The mock verifier does not check the event; this pins the
    ///      *contract* re-prove path. Partner circuit: dense chain ≤ 11 rungs.
    function test_reproveAgainstLaterInWindowAnchor_succeeds() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 2 * UsdcTestLib.UNIT);

        uint256 originalL1 = _submitBlock(1);
        for (uint256 i = 2; i <= WINDOW; i++) {
            _submitBlock(i);
        }
        _submitBlock(WINDOW + 1);
        assertFalse(bridge.isKnownLayerAnchor(1, originalL1), "original evicted");

        uint256 laterL1 = _layers(2)[0];
        assertTrue(bridge.isKnownLayerAnchor(1, laterL1), "block-2 L1 still in window");

        uint256[] memory pub = _withdrawPub(laterL1, 0xBEEF, 1 * UsdcTestLib.UNIT);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        assertTrue(
            bridge.withdrawByProofBundle(pub, hex"00", hopPubs, hopProofs),
            "re-prove against later anchor"
        );
        assertEq(usdc.balanceOf(RECIPIENT), 1 * UsdcTestLib.UNIT);
        assertTrue(bridge.isNullifierUsed(0xBEEF));
    }

    function _withdrawPub(uint256 finalRoot, uint256 nullifier, uint256 amount)
        internal
        view
        returns (uint256[] memory pub)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        pub = new uint256[](13);
        pub[0]  = 0;                    // tokenId
        pub[1]  = amount;
        pub[2]  = a >> 80;              // recipientHi
        pub[3]  = a & ((1 << 80) - 1);  // recipientLo
        pub[4]  = block.chainid;
        pub[5]  = 1;                    // senderAccFr
        pub[6]  = DAPP_FR;
        pub[7]  = ACC_FR;
        pub[8]  = nullifier;
        pub[9]  = finalRoot;
        pub[10] = 1;                    // anchorLayer
        pub[11] = SAME_THREAD_BLOCK;    // xBlockId
        pub[12] = SAME_THREAD_BLOCK;    // yBlockId (same-thread: x == y)
    }
}
