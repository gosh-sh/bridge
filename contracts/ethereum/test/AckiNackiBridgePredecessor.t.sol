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
import { MockAaveLib } from "./helpers/MockAaveLib.sol";
import { OptionalReturnERC20 } from "./AckiNackiBridgeTokenRegistry.t.sol";

/// @title AckiNackiBridgePredecessorTest
/// @notice Two bridges on one chain for the same AN-side identity: a
///         withdrawal pays only on the bridge its proof names (`dstBridge`),
///         and a successor takes over its predecessor's treasury.
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
        MockAaveLib.pool(address(usdc));
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
            MockAaveLib.pool(address(usdc)),
            MockAaveLib.aToken(address(usdc)),
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

    function _pubFor(address dstBridge, uint256 nullifier)
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
            anchorLayer: 1,
            dstBridge: uint256(uint160(dstBridge))
        });
    }

    /// @dev Pays on `b` a proof that names `b`.
    function _pay(AckiNackiBridge b, uint256 nullifier) internal {
        b.withdrawByProof(hex"c4", _pubFor(address(b), nullifier));
    }

    function _expectRefused(AckiNackiBridge b, uint256 nullifier) internal {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, nullifier)
        );
        _pay(b, nullifier);
    }

    /// @dev Submits to `b` a proof that names `named`; expects `WrongDstBridge`.
    function _expectWrongBridge(AckiNackiBridge b, address named, uint256 nullifier) internal {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.WrongDstBridge.selector, uint256(uint160(named)))
        );
        b.withdrawByProof(hex"c4", _pubFor(named, nullifier));
    }

    // ─────────────────────────────────────────────────────────────────────
    // A proof pays on the bridge it names, and nowhere else
    // ─────────────────────────────────────────────────────────────────────

    function test_paysAndRefusesItsOwnReplay() public {
        AckiNackiBridge b = _liveBridge(address(0));
        assertEq(address(b.predecessor()), address(0));
        assertFalse(b.isNullifierUsed(11));

        _pay(b, 11);
        assertEq(usdc.balanceOf(RECIPIENT), AMOUNT);
        assertTrue(b.isNullifierUsed(11));
        _expectRefused(b, 11);
    }

    /// @dev Two live bridges for one AN identity on one chain, neither paused:
    ///      each pays only the proofs that name it.
    function test_proofForOneBridgeDoesNotPayOnAnother() public {
        AckiNackiBridge first = _liveBridge(address(0));
        AckiNackiBridge second = _liveBridge(address(0));

        _pay(first, 21);
        _expectWrongBridge(second, address(first), 21);
        assertFalse(second.isNullifierUsed(21));

        _pay(second, 22);
        _expectWrongBridge(first, address(second), 22);
        assertEq(usdc.balanceOf(RECIPIENT), 2 * AMOUNT);
    }

    /// @dev The case bridge-audit#140 is about: a successor deployed next to a
    ///      bridge that already paid. The old proof names the old bridge, so
    ///      the successor refuses it without consulting the old bridge and
    ///      without the old bridge being paused.
    function test_successorRefusesWhatPredecessorPaid() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        _pay(oldBridge, 31);
        AckiNackiBridge newBridge = _liveBridge(address(oldBridge));
        assertFalse(oldBridge.paused());

        uint256 treasuryBefore = newBridge.treasuryBalance();
        _expectWrongBridge(newBridge, address(oldBridge), 31);
        assertEq(newBridge.treasuryBalance(), treasuryBefore);
        assertEq(usdc.balanceOf(RECIPIENT), AMOUNT);

        // And the other way: a burn bound to the successor does not pay on the
        // old bridge, paused or not.
        _expectWrongBridge(oldBridge, address(newBridge), 32);
        _pay(newBridge, 32);
        assertEq(usdc.balanceOf(RECIPIENT), 2 * AMOUNT);
    }

    function testFuzz_refusesEveryOtherDstBridge(uint256 dstBridge) public {
        AckiNackiBridge b = _liveBridge(address(0));
        vm.assume(dstBridge != uint256(uint160(address(b))));
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFor(address(b), 41);
        pub.dstBridge = dstBridge;
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.WrongDstBridge.selector, dstBridge));
        b.withdrawByProof(hex"c4", pub);
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

    function test_constructor_rejectsContractWithoutAccFrView() public {
        DappFrOnlyContract c = new DappFrOnlyContract(DAPP_FR);
        _expectInvalid(address(c));
        _newBridge(_withdrawConfig(address(c), DAPP_FR, ACC_FR));
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
            MockAaveLib.pool(address(usdc)),
            MockAaveLib.aToken(address(usdc)),
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

    /// @dev Old bridge paid one withdrawal and is paused; successor is deployed.
    function _cutover() internal returns (AckiNackiBridge oldBridge, AckiNackiBridge newBridge) {
        oldBridge = _liveBridge(address(0));
        _pay(oldBridge, 71);
        vm.startPrank(oldBridge.owner());
        oldBridge.pause();
        // OWN-8: the book has to be liquid before it can move.
        oldBridge.withdrawFromAave(type(uint256).max);
        vm.stopPrank();
        newBridge = _newBridge(_withdrawConfig(address(oldBridge), DAPP_FR, ACC_FR));
    }

    function test_migrateTreasury_movesBookToSuccessor() public {
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
        _expectWrongBridge(newBridge, address(oldBridge), 71);
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
        address eoa = makeAddr("eoa");
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, eoa));
        oldBridge.migrateTreasury(eoa);
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

    // ──────────────────────────────────────────────────────────────────────
    // A successor takes over a second token's custody and liabilities
    // ──────────────────────────────────────────────────────────────────────

    event TokenFunded(
        address indexed from, uint32 indexed tokenId, uint256 amount, uint256 liabilitiesAfter
    );
    event TokenMigrated(address indexed successor, uint32 indexed tokenId, uint256 amount);

    uint32 internal constant SECOND_ID = 4;
    uint256 internal constant SECOND_AMOUNT = 1e18;

    function _listSecond(AckiNackiBridge b, MockERC20 token, uint256 maxTransfer) internal {
        b.listToken(address(token), SECOND_ID, 18, maxTransfer, 10e18);
    }

    function _secondOnOldBridge() internal returns (AckiNackiBridge oldBridge, MockERC20 second) {
        oldBridge = _liveBridge(address(0));
        second = new MockERC20("Second", "SECOND", 18);
        _listSecond(oldBridge, second, SECOND_AMOUNT);
        oldBridge.setTokenDepositsEnabled(SECOND_ID, true);
        second.mint(address(this), SECOND_AMOUNT);
        second.approve(address(oldBridge), SECOND_AMOUNT);
        oldBridge.deposit(address(second), SECOND_AMOUNT, bytes32(uint256(1)));
        oldBridge.pause();
    }

    function _secondPub(address dstBridge, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub)
    {
        pub = _pubFor(dstBridge, nullifier);
        pub.tokenId = SECOND_ID;
        pub.amount = SECOND_AMOUNT;
    }

    function test_migrateToken_burnAfterTheSwitchPaysOnSuccessor() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        second.mint(address(oldBridge), 3);
        uint256 countBefore = next.depositCounter();

        vm.expectEmit(true, true, false, true, address(next));
        emit TokenFunded(address(oldBridge), SECOND_ID, SECOND_AMOUNT, SECOND_AMOUNT);
        vm.expectEmit(true, true, false, true, address(oldBridge));
        emit TokenMigrated(address(next), SECOND_ID, SECOND_AMOUNT);
        oldBridge.migrateToken(SECOND_ID, address(next));

        assertEq(oldBridge.liabilitiesOf(SECOND_ID), 0);
        assertEq(second.balanceOf(address(oldBridge)), 3);
        assertEq(second.allowance(address(oldBridge), address(next)), 0);
        assertEq(next.liabilitiesOf(SECOND_ID), SECOND_AMOUNT);
        assertEq(second.balanceOf(address(next)), SECOND_AMOUNT);
        assertEq(next.depositCounter(), countBefore);

        next.withdrawByProof(hex"c4", _secondPub(address(next), 991));
        assertEq(second.balanceOf(RECIPIENT), SECOND_AMOUNT);
        assertEq(next.liabilitiesOf(SECOND_ID), 0);

        oldBridge.unpause();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WrongDstBridge.selector, uint256(uint160(address(next)))
            )
        );
        oldBridge.withdrawByProof(hex"c4", _secondPub(address(next), 992));
        oldBridge.delistToken(SECOND_ID);
    }

    function test_migrateToken_movesSeveralTokensAndUsdcInOneCutover() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        oldBridge.unpause();
        MockERC20 third = new MockERC20("Third", "THIRD", 8);
        uint32 thirdId = 5;
        oldBridge.listToken(address(third), thirdId, 8, 2e8, 20e8);
        oldBridge.setTokenDepositsEnabled(thirdId, true);
        third.mint(address(this), 2e8);
        third.approve(address(oldBridge), 2e8);
        oldBridge.deposit(address(third), 2e8, bytes32(uint256(2)));
        oldBridge.pause();
        oldBridge.withdrawFromAave(type(uint256).max);

        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        next.listToken(address(third), thirdId, 8, 2e8, 20e8);
        uint256 oldBook = oldBridge.treasuryBalance();
        uint256 newBook = next.treasuryBalance();

        oldBridge.migrateTreasury(address(next));
        oldBridge.migrateToken(SECOND_ID, address(next));
        oldBridge.migrateToken(thirdId, address(next));

        assertEq(oldBridge.treasuryBalance(), 0);
        assertEq(oldBridge.liabilitiesOf(SECOND_ID), 0);
        assertEq(oldBridge.liabilitiesOf(thirdId), 0);
        assertEq(next.treasuryBalance(), newBook + oldBook);
        assertEq(next.liabilitiesOf(SECOND_ID), SECOND_AMOUNT);
        assertEq(next.liabilitiesOf(thirdId), 2e8);
        assertEq(second.balanceOf(address(next)), SECOND_AMOUNT);
        assertEq(third.balanceOf(address(next)), 2e8);

        next.withdrawByProof(hex"c4", _secondPub(address(next), 993));
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFor(address(next), 994);
        pub.tokenId = thirdId;
        pub.amount = 2e8;
        next.withdrawByProof(hex"c4", pub);
        assertEq(second.balanceOf(RECIPIENT), SECOND_AMOUNT);
        assertEq(third.balanceOf(RECIPIENT), 2e8);
    }

    function test_migrateToken_leavesTheUsdcBookAlone() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        uint256 oldBook = oldBridge.treasuryBalance();
        uint256 newBook = next.treasuryBalance();

        oldBridge.migrateToken(SECOND_ID, address(next));

        assertEq(oldBridge.treasuryBalance(), oldBook);
        assertEq(next.treasuryBalance(), newBook);
    }

    function test_migrateToken_requiresPause() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        oldBridge.unpause();
        vm.expectRevert(AckiNackiBridge.NotPaused.selector);
        oldBridge.migrateToken(SECOND_ID, address(next));
    }

    function test_migrateToken_onlyOwner() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        vm.prank(funder);
        vm.expectRevert();
        oldBridge.migrateToken(SECOND_ID, address(next));
        assertEq(oldBridge.liabilitiesOf(SECOND_ID), SECOND_AMOUNT);
    }

    function test_migrateToken_refusesBridgeThatDoesNotNameIt() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge stranger = _liveBridge(address(0));
        _listSecond(stranger, second, SECOND_AMOUNT);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(stranger))
        );
        oldBridge.migrateToken(SECOND_ID, address(stranger));
    }

    function test_migrateToken_refusesUsdc() public {
        (AckiNackiBridge oldBridge,) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        uint32 usdcId = oldBridge.usdcTokenId();
        vm.expectRevert(AckiNackiBridge.CannotMigrateUsdc.selector);
        oldBridge.migrateToken(usdcId, address(next));
    }

    function test_migrateToken_refusesUnlistedId() public {
        (AckiNackiBridge oldBridge,) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, uint32(77))
        );
        oldBridge.migrateToken(77, address(next));
    }

    function test_migrateToken_refusesSuccessorThatDidNotListIt() public {
        (AckiNackiBridge oldBridge,) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.SuccessorWithdrawalCapTooLow.selector, 0, SECOND_AMOUNT
            )
        );
        oldBridge.migrateToken(SECOND_ID, address(next));
    }

    function test_migrateToken_refusesLowerWithdrawalCap() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT - 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.SuccessorWithdrawalCapTooLow.selector,
                SECOND_AMOUNT - 1,
                SECOND_AMOUNT
            )
        );
        oldBridge.migrateToken(SECOND_ID, address(next));
        assertEq(oldBridge.liabilitiesOf(SECOND_ID), SECOND_AMOUNT);
    }

    function test_migrateToken_refusesAnotherTokenUnderTheSameId() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        MockERC20 other = new MockERC20("Other", "OTHER", 18);
        _listSecond(next, other, SECOND_AMOUNT);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.FundedTokenMismatch.selector, SECOND_ID, address(second)
            )
        );
        oldBridge.migrateToken(SECOND_ID, address(next));
    }

    function test_migrateToken_twiceHasNothingToMove() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        oldBridge.migrateToken(SECOND_ID, address(next));
        vm.expectRevert(AckiNackiBridge.NothingToMigrate.selector);
        oldBridge.migrateToken(SECOND_ID, address(next));
    }

    function test_migrateToken_movesTokenThatReturnsNothing() public {
        AckiNackiBridge oldBridge = _liveBridge(address(0));
        OptionalReturnERC20 quiet = new OptionalReturnERC20();
        oldBridge.listToken(address(quiet), SECOND_ID, 6, 1e6, 10e6);
        oldBridge.setTokenDepositsEnabled(SECOND_ID, true);
        quiet.mint(address(this), 1e6);
        quiet.approve(address(oldBridge), 1e6);
        oldBridge.deposit(address(quiet), 1e6, bytes32(uint256(1)));
        oldBridge.pause();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        next.listToken(address(quiet), SECOND_ID, 6, 1e6, 10e6);

        oldBridge.migrateToken(SECOND_ID, address(next));

        assertEq(next.liabilitiesOf(SECOND_ID), 1e6);
        assertEq(quiet.balanceOf(address(next)), 1e6);
        assertEq(quiet.balanceOf(address(oldBridge)), 0);
    }

    function test_fundToken_refusesOthers() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        vm.prank(funder);
        vm.expectRevert(AckiNackiBridge.NotOwnerOrPredecessor.selector);
        next.fundToken(SECOND_ID, address(second), 1);
    }

    function test_fundToken_refusesUsdcAndZero() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        _listSecond(next, second, SECOND_AMOUNT);
        uint32 usdcId = next.usdcTokenId();
        vm.expectRevert(AckiNackiBridge.CannotMigrateUsdc.selector);
        next.fundToken(usdcId, address(usdc), 1);
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        next.fundToken(SECOND_ID, address(second), 0);
    }

    function test_fundToken_ownerCoversAboveTvlCapAndDepositsStayCapped() public {
        (AckiNackiBridge oldBridge, MockERC20 second) = _secondOnOldBridge();
        AckiNackiBridge next = _liveBridge(address(oldBridge));
        next.listToken(address(second), SECOND_ID, 18, 1e18, 1e18);
        next.setTokenDepositsEnabled(SECOND_ID, true);
        second.mint(address(this), 3e18);
        second.approve(address(next), 3e18);

        next.fundToken(SECOND_ID, address(second), 2e18);
        assertEq(next.liabilitiesOf(SECOND_ID), 2e18);

        vm.expectRevert(AckiNackiBridge.TvlCapReached.selector);
        next.deposit(address(second), 1e18, bytes32(uint256(1)));
    }
}
