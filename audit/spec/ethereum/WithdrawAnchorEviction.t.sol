// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/helpers/Bn254FrLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockBridgeWithdrawalVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title WithdrawAnchorEvictionTest
/// @notice Phase C / A3 — WD-Q1 / WD-6: L1 anchor evicted after 128 newer verifyBlocks.
/// @dev INV: WD-6 — rolling window HISTORY_PROOF_WINDOW = 128
contract WithdrawAnchorEvictionTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xE0100001;
    uint64 internal constant FIRST_SEQ_NO = 1;
    uint256 internal constant WINDOW = 128;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    address internal funder = address(0xF00D);
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

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
            VerifyBlockConfigLib.withWithdraw(IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR)
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

    /// @dev QC: WD-Q1 — stale finalRoot becomes UnknownAnchor after window eviction.
    function test_withdrawByProof_evictedAnchor_reverts() public {
        uint256 evictedL1 = _submitBlock(1);
        assertTrue(bridge.isKnownLayerAnchor(1, evictedL1), "pre: first anchor known");

        for (uint256 i = 2; i <= WINDOW; i++) {
            _submitBlock(i);
        }
        assertTrue(bridge.isKnownLayerAnchor(1, evictedL1), "pre: anchor still in full window");

        _submitBlock(WINDOW + 1);
        assertFalse(bridge.isKnownLayerAnchor(1, evictedL1), "post: oldest L1 evicted");
        assertEq(bridge.layerWindowLen(1), WINDOW, "ring stays full after wrap");

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1,
            recipientHi: 0,
            recipientLo: uint256(uint160(address(0x1111))),
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: 0xDEAD,
            finalRoot: evictedL1
        });

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnknownAnchor.selector, evictedL1));
        bridge.withdrawByProof(hex"00", pub);
    }

    /// @dev ETH-3: a `blockSeqNo` jump writes one window slot. The earlier
    ///      L1 hash stays known — unbounded seq_no is catch-up, not eviction.
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
        assertTrue(bridge.isKnownLayerAnchor(1, firstL1), "seq_no jump must not evict the previous L1 hash");
        assertTrue(bridge.isKnownLayerAnchor(1, jumped[0]), "jumped head recorded");
    }

    /// @dev ETH-3 / WD-Q1: after the original `finalRoot` is evicted, a new
    ///      Circuit 4 proof bound to a still-in-window descendant must pay.
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

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _withdrawPub(laterL1, 0xBEEF);
        assertTrue(bridge.withdrawByProof(hex"00", pub), "re-prove against later anchor");
        assertEq(usdc.balanceOf(RECIPIENT), 1 * UsdcTestLib.UNIT);
        assertTrue(bridge.isNullifierUsed(0xBEEF));
    }

    function _withdrawPub(uint256 finalRoot, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1 * UsdcTestLib.UNIT,
            recipientHi: a >> 80,
            recipientLo: a & ((1 << 80) - 1),
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: finalRoot
        });
    }
}
