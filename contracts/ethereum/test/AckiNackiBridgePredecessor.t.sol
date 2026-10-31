// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IPredecessorBridge.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockERC20.sol";
import "./mocks/MockPredecessorBridge.sol";

/// @title AckiNackiBridgePredecessorTest
/// @notice A bridge that replaces another on the same chain refuses the
///         withdrawals its predecessor line already paid, and only accepts a
///         predecessor that is a bridge for the same AN-side identity.
contract AckiNackiBridgePredecessorTest is Test {
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint256 internal constant AMOUNT = 5 * UsdcTestLib.UNIT;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    address internal funder = address(0xF00D);

    uint256 internal anchor;

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
        anchor = Bn254FrLib.toFr(uint256(keccak256("predecessor-anchor")));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    function _withdrawConfig(address predecessor, uint256 dappFr, uint256 accFr)
        internal
        view
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return VerifyBlockConfigLib.withWithdrawAfter(
            IBridgeWithdrawalVerifier(address(withdrawalVerifier)), dappFr, accFr, predecessor
        );
    }

    function _newBridge(AckiNackiBridge.BridgeWithdrawConfig memory bw)
        internal
        returns (AckiNackiBridge)
    {
        return new AckiNackiBridge(
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
            bw
        );
    }

    /// @dev A funded bridge with one anchored block, ready to pay.
    function _liveBridge(address predecessor) internal returns (AckiNackiBridge b) {
        b = _newBridge(_withdrawConfig(predecessor, DAPP_FR, ACC_FR));
        UsdcTestLib.depositUsdc(vm, usdc, b, funder, 40 * UsdcTestLib.UNIT);
        uint256[10] memory layers;
        layers[0] = anchor;
        b.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            1,
            layers,
            b.expectedPrevAnchor(1)
        );
    }

    function _pub(uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: AMOUNT,
            recipientHi: a >> 80,
            recipientLo: a & ((1 << 80) - 1),
            dstChainId: block.chainid,
            senderAccFr: 7,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: anchor,
            anchorLayer: 1
        });
    }

    function _pay(AckiNackiBridge b, uint256 nullifier) internal {
        b.withdrawByProof(hex"c4", _pub(nullifier));
    }

    function _expectRefused(AckiNackiBridge b, uint256 nullifier) internal {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, nullifier)
        );
        _pay(b, nullifier);
    }

    // ─────────────────────────────────────────────────────────────────────
    // No predecessor
    // ─────────────────────────────────────────────────────────────────────

    function test_zeroPredecessor_paysAndRefusesOnlyItsOwnReplay() public {
        AckiNackiBridge b = _liveBridge(address(0));
        assertEq(address(b.predecessor()), address(0));
        assertFalse(b.isNullifierUsed(11));

        _pay(b, 11);
        assertEq(usdc.balanceOf(RECIPIENT), AMOUNT);
        assertTrue(b.isNullifierUsed(11));
        _expectRefused(b, 11);
    }

    // ─────────────────────────────────────────────────────────────────────
    // One predecessor
    // ─────────────────────────────────────────────────────────────────────

    function test_successorRefusesWithdrawalPaidByPredecessor() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        _pay(oldBridge, 21);
        vm.prank(oldBridge.owner());
        oldBridge.pause();

        AckiNackiBridge newBridge = _liveBridge(address(oldBridge));
        assertEq(address(newBridge.predecessor()), address(oldBridge));
        assertTrue(newBridge.isNullifierUsed(21));

        uint256 treasuryBefore = newBridge.treasuryBalance();
        _expectRefused(newBridge, 21);
        assertEq(newBridge.treasuryBalance(), treasuryBefore);
        assertEq(usdc.balanceOf(RECIPIENT), AMOUNT);
    }

    function test_successorPaysWithdrawalPredecessorNeverPaid() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        _pay(oldBridge, 31);
        vm.prank(oldBridge.owner());
        oldBridge.pause();
        AckiNackiBridge newBridge = _liveBridge(address(oldBridge));

        _pay(newBridge, 32);
        assertEq(usdc.balanceOf(RECIPIENT), 2 * AMOUNT);
        assertTrue(newBridge.isNullifierUsed(32));
        _expectRefused(newBridge, 32);
        // The predecessor does not consult its successor; it has to stay paused.
        assertFalse(oldBridge.isNullifierUsed(32));
    }

    /// @dev A predecessor that stops answering blocks payouts instead of
    ///      letting them through unchecked.
    function test_revertingPredecessorBlocksPayout() public {
        MockPredecessorBridge p = new MockPredecessorBridge(DAPP_FR, ACC_FR);
        AckiNackiBridge b = _liveBridge(address(p));
        p.setReverts(true);
        vm.expectRevert(bytes("predecessor down"));
        _pay(b, 41);
        vm.expectRevert(bytes("predecessor down"));
        b.isNullifierUsed(41);

        p.setReverts(false);
        _pay(b, 41);
        assertEq(usdc.balanceOf(RECIPIENT), AMOUNT);
    }

    /// @dev The successor reads only the three views, so a bridge that has no
    ///      `predecessor()` of its own (every deployment before this field)
    ///      is a valid predecessor.
    function test_predecessorWithoutOwnPredecessorView() public {
        MockPredecessorBridge p = new MockPredecessorBridge(DAPP_FR, ACC_FR);
        p.setUsed(51);
        AckiNackiBridge b = _liveBridge(address(p));
        _expectRefused(b, 51);
        _pay(b, 52);
    }

    function testFuzz_successorRefusesEveryPredecessorNullifier(uint256 nullifier) public {
        nullifier = bound(nullifier, 0, Bn254FrLib.R - 1);
        MockPredecessorBridge p = new MockPredecessorBridge(DAPP_FR, ACC_FR);
        p.setUsed(nullifier);
        AckiNackiBridge b = _liveBridge(address(p));
        assertTrue(b.isNullifierUsed(nullifier));
        _expectRefused(b, nullifier);
    }

    // ─────────────────────────────────────────────────────────────────────
    // A line of predecessors
    // ─────────────────────────────────────────────────────────────────────

    function test_thirdBridgeRefusesWhatEitherEarlierBridgePaid() public {
        AckiNackiBridge first = _liveBridge(address(0));
        _pay(first, 61);
        vm.prank(first.owner());
        first.pause();

        AckiNackiBridge second = _liveBridge(address(first));
        _pay(second, 62);
        vm.prank(second.owner());
        second.pause();

        AckiNackiBridge third = _liveBridge(address(second));
        assertTrue(third.isNullifierUsed(61));
        assertTrue(third.isNullifierUsed(62));
        _expectRefused(third, 61);
        _expectRefused(third, 62);

        _pay(third, 63);
        assertEq(usdc.balanceOf(RECIPIENT), 3 * AMOUNT);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Constructor validation
    // ─────────────────────────────────────────────────────────────────────

    function _expectInvalid(address predecessor) internal {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.InvalidPredecessor.selector, predecessor)
        );
    }

    function test_constructor_rejectsAddressWithoutCode() public {
        address eoa = address(0xBEEF);
        _expectInvalid(eoa);
        _newBridge(_withdrawConfig(eoa, DAPP_FR, ACC_FR));
    }

    function test_constructor_rejectsContractThatIsNotABridge() public {
        _expectInvalid(address(usdc));
        _newBridge(_withdrawConfig(address(usdc), DAPP_FR, ACC_FR));
    }

    function test_constructor_rejectsContractWithoutIdentityViews() public {
        NullifierOnlyContract c = new NullifierOnlyContract();
        _expectInvalid(address(c));
        _newBridge(_withdrawConfig(address(c), DAPP_FR, ACC_FR));
    }

    function test_constructor_rejectsRevertingPredecessor() public {
        MockPredecessorBridge p = new MockPredecessorBridge(DAPP_FR, ACC_FR);
        p.setReverts(true);
        _expectInvalid(address(p));
        _newBridge(_withdrawConfig(address(p), DAPP_FR, ACC_FR));
    }

    function test_constructor_rejectsPredecessorOfAnotherAnBridge() public {
        AckiNackiBridge other = _newBridge(_withdrawConfig(address(0), DAPP_FR, ACC_FR + 1));
        _expectInvalid(address(other));
        _newBridge(_withdrawConfig(address(other), DAPP_FR, ACC_FR));

        AckiNackiBridge otherDapp = _newBridge(_withdrawConfig(address(0), DAPP_FR + 1, ACC_FR));
        _expectInvalid(address(otherDapp));
        _newBridge(_withdrawConfig(address(otherDapp), DAPP_FR, ACC_FR));
    }

    function test_constructor_rejectsPredecessorWithWithdrawDisabled() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        AckiNackiBridge.BridgeWithdrawConfig memory bw = VerifyBlockConfigLib.disabledWithdraw();
        bw.predecessor = IPredecessorBridge(address(oldBridge));
        _expectInvalid(address(oldBridge));
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            bw
        );
    }

    function test_constructor_acceptsBridgeForSameIdentity() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        AckiNackiBridge b = _newBridge(_withdrawConfig(address(oldBridge), DAPP_FR, ACC_FR));
        assertEq(address(b.predecessor()), address(oldBridge));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Treasury handover
    // ─────────────────────────────────────────────────────────────────────

    /// @dev Old bridge paid one withdrawal and is paused; successor is live.
    function _cutover() internal returns (AckiNackiBridge oldBridge, AckiNackiBridge newBridge) {
        oldBridge = _liveBridge(address(0));
        _pay(oldBridge, 71);
        vm.prank(oldBridge.owner());
        oldBridge.pause();
        newBridge = _newBridge(_withdrawConfig(address(oldBridge), DAPP_FR, ACC_FR));
    }

    function test_migrateTreasury_movesBookAndPaysOldBurnsFromIt() public {
        (AckiNackiBridge oldBridge, AckiNackiBridge newBridge) = _cutover();
        uint256 owed = oldBridge.treasuryBalance();
        assertEq(owed, 40 * UsdcTestLib.UNIT - AMOUNT);
        // Stray USDC above the book stays behind for the owner to skim.
        usdc.mint(address(oldBridge), 3);

        vm.prank(oldBridge.owner());
        oldBridge.migrateTreasury(address(newBridge));

        assertEq(oldBridge.treasuryBalance(), 0);
        assertEq(usdc.balanceOf(address(oldBridge)), 3);
        assertEq(oldBridge.excessUsdc(), 3);
        assertEq(newBridge.treasuryBalance(), owed);
        assertEq(usdc.balanceOf(address(newBridge)), owed);
        assertEq(newBridge.depositCounter(), 0);

        uint256[10] memory layers;
        layers[0] = anchor;
        newBridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            1,
            layers,
            newBridge.expectedPrevAnchor(1)
        );
        _expectRefused(newBridge, 71);
        _pay(newBridge, 72);
        assertEq(newBridge.treasuryBalance(), owed - AMOUNT);

        // A paused-then-unpaused old bridge has nothing left to pay from.
        vm.startPrank(oldBridge.owner());
        oldBridge.unpause();
        vm.stopPrank();
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.WithdrawTreasuryShortfall.selector, AMOUNT, 0)
        );
        _pay(oldBridge, 73);
    }

    function test_migrateTreasury_requiresPause() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        AckiNackiBridge newBridge = _newBridge(_withdrawConfig(address(oldBridge), DAPP_FR, ACC_FR));
        vm.prank(oldBridge.owner());
        vm.expectRevert(AckiNackiBridge.NotPaused.selector);
        oldBridge.migrateTreasury(address(newBridge));
    }

    function test_migrateTreasury_onlyOwner() public {
        (AckiNackiBridge oldBridge, AckiNackiBridge newBridge) = _cutover();
        vm.prank(address(0xBAD));
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        oldBridge.migrateTreasury(address(newBridge));
    }

    function test_migrateTreasury_refusesBridgeThatDoesNotNameIt() public {
        (AckiNackiBridge oldBridge,) = _cutover();
        AckiNackiBridge stranger = _liveBridge(address(0));
        vm.startPrank(oldBridge.owner());
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(stranger))
        );
        oldBridge.migrateTreasury(address(stranger));
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(usdc))
        );
        oldBridge.migrateTreasury(address(usdc));
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(0)));
        oldBridge.migrateTreasury(address(0));
        vm.stopPrank();
    }

    function test_migrateTreasury_twiceHasNothingToMove() public {
        (AckiNackiBridge oldBridge, AckiNackiBridge newBridge) = _cutover();
        vm.startPrank(oldBridge.owner());
        oldBridge.migrateTreasury(address(newBridge));
        vm.expectRevert(AckiNackiBridge.NothingToMigrate.selector);
        oldBridge.migrateTreasury(address(newBridge));
        vm.stopPrank();
    }

    function test_fundTreasury_ownerCoversPredecessorThatCannotMigrate() public {
        MockPredecessorBridge p = new MockPredecessorBridge(DAPP_FR, ACC_FR);
        AckiNackiBridge b = _newBridge(_withdrawConfig(address(p), DAPP_FR, ACC_FR));
        address owner_ = b.owner();
        usdc.mint(owner_, 7 * UsdcTestLib.UNIT);
        vm.startPrank(owner_);
        usdc.approve(address(b), 7 * UsdcTestLib.UNIT);
        b.fundTreasury(7 * UsdcTestLib.UNIT);
        vm.stopPrank();
        assertEq(b.treasuryBalance(), 7 * UsdcTestLib.UNIT);
        assertEq(b.depositCounter(), 0);
    }

    function test_fundTreasury_refusesOthers() public {
        AckiNackiBridge b = _liveBridge(address(0));
        usdc.mint(address(0xBAD), 1);
        vm.startPrank(address(0xBAD));
        usdc.approve(address(b), 1);
        vm.expectRevert(AckiNackiBridge.NotOwnerOrPredecessor.selector);
        b.fundTreasury(1);
        vm.stopPrank();
    }
}
