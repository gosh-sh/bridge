// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./mocks/MockAave.sol";
import "./mocks/MockERC20.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";

/// @title AckiNackiBridgeAaveTest
/// @notice Exercises the AAVE USDC integration surface of AckiNackiBridge.
contract AckiNackiBridgeAaveTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;

    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal owner = address(this);
    address internal user1 = address(0xA1);
    address internal user2 = address(0xA2);
    address internal yieldSink = address(0xBEEF);

    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event WithdrawnFromAave(uint256 amountRequested, uint256 amountReceived);
    event YieldHarvested(address indexed recipient, uint256 amount);
    event EmergencyWithdrawAll(uint256 amount);
    event AaveEnabledSet(bool enabled);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event YieldRecipientSet(address indexed recipient);
    event UnbackedPrincipalWrittenOff(uint256 amount);

    function setUp() public {
        oracle = new MockBlockHeaderOracle();

        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    // -----------------------------------------------------------------
    // Constructor / config
    // -----------------------------------------------------------------

    function test_constructor_wiresAaveAndUsdc() public view {
        assertEq(address(bridge.usdc()), address(usdc), "usdc wired");
        assertEq(address(bridge.aavePool()), address(pool), "pool wired");
        assertEq(address(bridge.aUSDC()), address(aUSDC), "aUSDC wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default when wired");
        assertEq(bridge.owner(), owner, "owner");
        assertEq(bridge.yieldRecipient(), owner, "yield recipient defaults to owner");
        assertEq(bridge.liquidReserveBps(), 1_000, "default 10% reserve");
        assertEq(bridge.MAX_DEPOSIT_AMOUNT(), type(uint64).max, "u64 mint-path cap");
    }

    function test_constructor_partialAaveWiringReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidAaveAddress.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_constructor_zeroUsdcReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidUsdc.selector);
        new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_constructor_noAaveIsLegal() public {
        AckiNackiBridge plain = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        assertFalse(plain.aaveEnabled(), "aave disabled when no addresses");
        assertEq(address(plain.aavePool()), address(0));
    }

    // -----------------------------------------------------------------
    // supplyToAave
    // -----------------------------------------------------------------

    function test_supplyToAave_respectsLiquidReserve() public {
        uint256 depositAmount = 10 * UsdcTestLib.UNIT;
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, depositAmount);

        uint256 reserve = 1 * UsdcTestLib.UNIT;
        uint256 supplyable = 9 * UsdcTestLib.UNIT;
        vm.expectEmit(false, false, false, true);
        emit SuppliedToAave(supplyable, supplyable);
        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.suppliedPrincipal(), supplyable, "principal");
        assertEq(bridge.aUsdcBalance(), supplyable, "aUSDC balance");
        assertEq(usdc.balanceOf(address(bridge)), reserve, "liquid reserve kept");
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury unchanged");
    }

    function test_supplyToAave_explicitAmount() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(5 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 5 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 5 * UsdcTestLib.UNIT);
    }

    function test_supplyToAave_onlyOwner() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_whenDisabledReverts() public {
        bridge.setAaveEnabled(false);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.AaveDisabled.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_nothingToSupplyReverts() public {
        vm.expectRevert(AckiNackiBridge.NothingToSupply.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_amountExceedsAvailableReverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.supplyToAave(10 * UsdcTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // withdrawFromAave (owner preemptive top-up)
    // -----------------------------------------------------------------

    function test_withdrawFromAave_preemptivelyTopsUp() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        bridge.withdrawFromAave(3 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 6 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 4 * UsdcTestLib.UNIT);
    }

    function test_withdrawFromAave_onlyOwner() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.withdrawFromAave(1 * UsdcTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // Yield
    // -----------------------------------------------------------------

    function test_accruedYield_reflectsAaveGrowth() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 500_000);

        assertEq(bridge.accruedYield(), 500_000, "accrued yield visible");
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT, "principal unchanged");
    }

    function test_harvestYield_sendsToRecipient() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 300_000);

        bridge.setYieldRecipient(yieldSink);

        uint256 sinkBefore = usdc.balanceOf(yieldSink);
        vm.expectEmit(true, false, false, true);
        emit YieldHarvested(yieldSink, 300_000);
        bridge.harvestYield(300_000);

        assertEq(usdc.balanceOf(yieldSink), sinkBefore + 300_000, "yield paid");
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT, "principal intact");
        assertEq(bridge.accruedYield(), 0, "yield consumed");
    }

    function test_harvestYield_partialHarvest() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 400_000);

        bridge.setYieldRecipient(yieldSink);
        bridge.harvestYield(100_000);
        assertEq(bridge.accruedYield(), 300_000, "remaining yield");
        assertEq(usdc.balanceOf(yieldSink), 100_000, "recipient paid");
    }

    function test_harvestYield_amountExceedsYieldReverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 100_000);

        vm.expectRevert(AckiNackiBridge.NoYield.selector);
        bridge.harvestYield(1 * UsdcTestLib.UNIT);
    }

    function test_harvestYield_onlyOwner() public {
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.harvestYield(1);
    }

    // -----------------------------------------------------------------
    // Solvency gate on harvestYield (P0)
    //
    // Reviewer scenario: after a socialized AAVE loss, `accruedYield()` is
    // purely book-side (aUSDC - suppliedPrincipal) and does NOT know whether
    // user principal (`treasuryBalance`) is still covered by liquid USDC +
    // aUSDC. If the owner wrote off unbacked principal and then interest
    // starts accruing, the fresh "yield" belongs to users first (refilling
    // the hole), not the yieldRecipient. `harvestYield` must bail out when
    // the transfer would leave backing < treasuryBalance.
    // -----------------------------------------------------------------

    /// @dev Setup: 10 USDC deposited, fully supplied to AAVE, 2 USDC worth of
    ///      aUSDC lost (socialized), owner writes off the unbacked book,
    ///      then AAVE accrues 3 units of interest. Backing is 8 + 3 = 11,
    ///      treasury is 10, so only 1 unit of yield is honestly harvestable.
    ///      harvestYield(3) must revert instead of re-opening the hole.
    function test_harvestYield_revertsWhenWouldBreakSolvency() public {
        uint256 deposit = 10 * UsdcTestLib.UNIT;
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, deposit);
        bridge.supplyToAave(type(uint256).max);

        // Socialized loss: 2 USDC of aUSDC disappear from the bridge.
        uint256 lost = 2 * UsdcTestLib.UNIT;
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), lost);

        // Only (deposit - liquidBuffer) was actually supplied to AAVE, so
        // after the socialized loss the real aUSDC balance is
        // (deposit - buffer - lost). Owner acknowledges the hole on the books.
        uint256 liquidBuffer = usdc.balanceOf(address(bridge));
        uint256 aUsdcAfterLoss = bridge.aUsdcBalance();
        assertEq(aUsdcAfterLoss, deposit - liquidBuffer - lost, "aUSDC = supplied - lost");

        bridge.writeOffUnbackedPrincipal();
        assertEq(bridge.suppliedPrincipal(), aUsdcAfterLoss, "book cut to real aUSDC");
        assertEq(bridge.treasuryBalance(), deposit, "user principal untouched");
        assertEq(bridge.accruedYield(), 0, "no yield yet");

        // AAVE starts paying interest again. On the books this looks like 3
        // units of harvestable yield, but 2 of them are needed to refill the
        // user-principal hole first.
        uint256 interest = 3 * UsdcTestLib.UNIT;
        aUSDC.accrueYield(address(bridge), interest);
        usdc.mint(address(pool), interest); // back the synthetic yield
        assertEq(bridge.accruedYield(), interest, "book-side yield = interest");

        bridge.setYieldRecipient(yieldSink);

        uint256 backing = usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance();
        assertEq(backing, liquidBuffer + aUsdcAfterLoss + interest, "backing = liquid + aUSDC");
        assertLt(
            backing, bridge.treasuryBalance() + interest, "draining full interest would under-back"
        );

        // Full harvest would widen the hole: revert.
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HarvestWouldBreakSolvency.selector,
                interest,
                backing,
                bridge.treasuryBalance()
            )
        );
        bridge.harvestYield(interest);

        // Nothing moved to yieldSink.
        assertEq(usdc.balanceOf(yieldSink), 0, "recipient not paid on revert");
        assertEq(bridge.aUsdcBalance(), aUsdcAfterLoss + interest, "aUSDC untouched");
    }

    /// @dev Same hole, but now AAVE accrues enough interest that even after
    ///      covering the 2-unit shortfall there is a 1-unit honest surplus.
    ///      `harvestYield(surplus)` must pass; the remaining `backing ==
    ///      treasuryBalance` is still fully solvent.
    function test_harvestYield_succeedsOnceInterestRefillsHole() public {
        uint256 deposit = 10 * UsdcTestLib.UNIT;
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, deposit);
        bridge.supplyToAave(type(uint256).max);

        uint256 lost = 2 * UsdcTestLib.UNIT;
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), lost);
        bridge.writeOffUnbackedPrincipal();

        // 3 units of interest = 2 refill the hole + 1 honest surplus.
        uint256 interest = 3 * UsdcTestLib.UNIT;
        aUSDC.accrueYield(address(bridge), interest);
        usdc.mint(address(pool), interest);

        bridge.setYieldRecipient(yieldSink);

        uint256 surplus = 1 * UsdcTestLib.UNIT;
        uint256 backingBefore = usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance();
        uint256 treasury = bridge.treasuryBalance();
        assertEq(backingBefore, treasury + surplus, "honest surplus = 1 UNIT");

        vm.expectEmit(true, false, false, true);
        emit YieldHarvested(yieldSink, surplus);
        bridge.harvestYield(surplus);

        assertEq(usdc.balanceOf(yieldSink), surplus, "only honest surplus paid");
        uint256 backingAfter = usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance();
        assertEq(backingAfter, treasury, "backing exactly covers users after harvest");

        // Any further harvest is blocked (would push backing below treasury).
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HarvestWouldBreakSolvency.selector, 1, backingAfter, treasury
            )
        );
        bridge.harvestYield(1);
    }

    // -----------------------------------------------------------------
    // Emergency
    // -----------------------------------------------------------------

    function test_emergencyWithdrawAll_pullsEverything() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 700_000);
        // Back the synthetic yield with underlying USDC in the pool.
        usdc.mint(address(pool), 700_000);

        vm.expectEmit(false, false, false, true);
        emit AaveEnabledSet(false);
        bridge.emergencyWithdrawAll();

        assertFalse(bridge.aaveEnabled(), "aave disabled");
        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        assertEq(bridge.aUsdcBalance(), 0, "aUSDC drained");
        assertEq(usdc.balanceOf(address(bridge)), 10_700_000, "all USDC home");
    }

    /// @dev A pool that leaves aUSDC after withdraw(max) must not let
    ///      emergency zero `suppliedPrincipal` — leftover shares would then
    ///      count as `accruedYield` and `harvestYield` would pay them out.
    function test_eth7_emergencyLeftoverAToken_reverts_andDoesNotOpenHarvest() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 principal = bridge.suppliedPrincipal();
        uint256 leftover = 1 * UsdcTestLib.UNIT;
        pool.setLeftoverOnMaxWithdraw(leftover);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.EmergencyLeftoverAToken.selector, leftover)
        );
        bridge.emergencyWithdrawAll();

        assertEq(bridge.suppliedPrincipal(), principal, "principal book unchanged");
        assertTrue(bridge.aaveEnabled(), "aave stays enabled on revert");
        assertEq(bridge.aUsdcBalance(), principal, "aUSDC still held (tx reverted)");
        // Honest yield path still only the delta above principal, not leftover principal.
        assertEq(bridge.accruedYield(), 0);
    }

    /// @notice Book the aUSDC delta, not the USDC sent. A pool that mints
    ///         fewer shares than it pulls must not inflate `suppliedPrincipal`
    ///         above `aUsdcBalance` — that residue cannot be withdrawn.
    function test_supplyToAave_booksATokenDelta() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        uint256 haircut = 10;
        pool.setSupplyHaircut(haircut);
        bridge.supplyToAave(5 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 5 * UsdcTestLib.UNIT - haircut);
        assertEq(bridge.aUsdcBalance(), bridge.suppliedPrincipal());
    }

    function test_supplyToAave_zeroDeltaRevertsAaveSupplyFailed() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        uint256 toSupply = 5 * UsdcTestLib.UNIT;
        pool.setSupplyHaircut(toSupply);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.AaveSupplyFailed.selector, toSupply, 0)
        );
        bridge.supplyToAave(toSupply);
    }

    /// @notice After a full aToken drain, keep `principal - received` on the
    ///         books instead of zeroing. Harvest still sees no yield.
    function test_emergencyWithdrawAll_keepsShortfallOnBooks() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 haircut = 10;
        pool.setRedeemHaircut(haircut);

        bridge.emergencyWithdrawAll();

        assertFalse(bridge.aaveEnabled());
        assertEq(bridge.aUsdcBalance(), 0);
        assertEq(bridge.suppliedPrincipal(), haircut, "shortfall stays booked");
        assertEq(bridge.accruedYield(), 0, "empty pool is not yield");
        assertEq(usdc.balanceOf(address(bridge)), 10 * UsdcTestLib.UNIT - haircut);

        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.withdrawFromAave(type(uint256).max);

        vm.expectEmit(false, false, false, true);
        emit UnbackedPrincipalWrittenOff(haircut);
        bridge.writeOffUnbackedPrincipal();
        assertEq(bridge.suppliedPrincipal(), 0);
    }

    function test_writeOffUnbackedPrincipal_revertsWhenATokenRemains() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        vm.expectRevert(AckiNackiBridge.NothingToWriteOff.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_withdrawFromAave_maxPullsBackedWhenUnderBooked() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 lost = 10;
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), lost);

        bridge.withdrawFromAave(type(uint256).max);
        assertEq(bridge.aUsdcBalance(), 0);
        assertEq(bridge.suppliedPrincipal(), lost);
    }

    function test_writeOffUnbackedPrincipal_clampsToDust() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 booked = bridge.suppliedPrincipal();
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), booked - 1);

        vm.expectEmit(false, false, false, true);
        emit UnbackedPrincipalWrittenOff(booked - 1);
        bridge.writeOffUnbackedPrincipal();
        assertEq(bridge.suppliedPrincipal(), 1);
        assertEq(bridge.aUsdcBalance(), 1);
        assertEq(bridge.accruedYield(), 0);
    }

    function test_writeOffUnbackedPrincipal_revertsWhenBooksAreClean() public {
        vm.expectRevert(AckiNackiBridge.NothingToWriteOff.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_writeOffUnbackedPrincipal_onlyOwner() public {
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_eth9_approveFalse_reverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        usdc.setApproveReturnsFalse(true);
        vm.expectRevert(AckiNackiBridge.ApproveFailed.selector);
        bridge.supplyToAave(type(uint256).max);
        assertEq(bridge.suppliedPrincipal(), 0, "no principal booked on failed approve");
    }

    // -----------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------

    function test_setLiquidReserveBps_capped() public {
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooHigh.selector);
        bridge.setLiquidReserveBps(5_001);
        bridge.setLiquidReserveBps(2_500);
        assertEq(bridge.liquidReserveBps(), 2_500);
    }

    function test_transferOwnership_flowsAllAuthorities() public {
        vm.expectEmit(true, true, false, true);
        emit OwnershipTransferStarted(address(this), user2);
        bridge.transferOwnership(user2);
        assertEq(bridge.owner(), address(this), "still old owner until accept");
        assertEq(bridge.pendingOwner(), user2);

        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        vm.prank(user2);
        bridge.setAaveEnabled(false);

        vm.expectRevert(AckiNackiBridge.OwnershipNotPending.selector);
        bridge.acceptOwnership();

        vm.expectEmit(true, true, false, true);
        emit OwnershipTransferred(address(this), user2);
        vm.expectEmit(true, false, false, true);
        emit YieldRecipientSet(user2);
        vm.prank(user2);
        bridge.acceptOwnership();
        assertEq(bridge.owner(), user2);
        assertEq(bridge.pendingOwner(), address(0));
        assertEq(bridge.yieldRecipient(), user2, "default recipient follows owner");

        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setAaveEnabled(false);
        vm.prank(user2);
        bridge.setAaveEnabled(false);
    }

    function test_acceptOwnership_movesDefaultYieldRecipient_harvestPaysNewOwner() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        aUSDC.accrueYield(address(bridge), 300_000);

        address compromised = address(this);
        bridge.transferOwnership(user2);
        vm.prank(user2);
        bridge.acceptOwnership();
        assertEq(bridge.yieldRecipient(), user2);

        uint256 newOwnerBefore = usdc.balanceOf(user2);
        uint256 oldOwnerBefore = usdc.balanceOf(compromised);
        vm.prank(user2);
        bridge.harvestYield(300_000);
        assertEq(usdc.balanceOf(user2), newOwnerBefore + 300_000, "harvest follows new owner");
        assertEq(usdc.balanceOf(compromised), oldOwnerBefore, "old owner must not receive yield");
    }

    function test_acceptOwnership_keepsExplicitYieldRecipient() public {
        bridge.setYieldRecipient(yieldSink);
        bridge.transferOwnership(user2);
        vm.prank(user2);
        bridge.acceptOwnership();
        assertEq(bridge.owner(), user2);
        assertEq(bridge.yieldRecipient(), yieldSink, "explicit recipient stays");
    }

    // -----------------------------------------------------------------
    // Accounting invariants
    // -----------------------------------------------------------------

    function testFuzz_totalAssetsCoversTreasury(uint96 depositAmt, uint16 bps) public {
        vm.assume(depositAmt > 0 && depositAmt <= 100 * UsdcTestLib.UNIT);
        vm.assume(bps <= bridge.MAX_LIQUID_RESERVE_BPS());

        bridge.setLiquidReserveBps(bps);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, depositAmt);

        uint256 reserve = (uint256(depositAmt) * bps) / bridge.BPS_DENOMINATOR();
        uint256 supplyable = depositAmt > reserve ? uint256(depositAmt) - reserve : 0;
        if (supplyable > 0) {
            bridge.supplyToAave(supplyable);
        }

        assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "solvent");
    }
}
