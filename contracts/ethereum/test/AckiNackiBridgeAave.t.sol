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
    event AaveSupplyRefused(uint256 amount, bytes reason);

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
        assertEq(bridge.supplyMultiplierBps(), 20_000, "default 2x threshold");
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

    /// @dev CFG-1: no bridge without AAVE, on any chain.
    function test_constructor_noAaveReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidAaveAddress.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    // -----------------------------------------------------------------
    // Supply in deposit
    // -----------------------------------------------------------------

    /// @dev DEP-3: the first deposit crosses the 2× threshold and leaves
    ///      the 10% target liquid.
    function test_deposit_crossingThresholdLeavesTarget() public {
        uint256 depositAmount = 10 * UsdcTestLib.UNIT;
        uint256 supplied = 9 * UsdcTestLib.UNIT;
        _fund(user1, depositAmount);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit SuppliedToAave(supplied, supplied);
        _depositFunded(user1, depositAmount);

        assertEq(bridge.suppliedPrincipal(), supplied, "principal");
        assertEq(bridge.aUsdcBalance(), supplied, "aUSDC balance");
        assertEq(usdc.balanceOf(address(bridge)), 1 * UsdcTestLib.UNIT, "target kept");
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury");
    }

    /// @dev DEP-3: the supply is exactly the liquid principal above the target.
    function test_deposit_suppliesLiquidAboveTarget() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        // L = 1 + 10 = 11, B = 20, T = 2, H = 4: supplies 9.
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 18 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 2 * UsdcTestLib.UNIT);
    }

    /// @dev CFG-9 / TC-13: no external function supplies the pool.
    function test_supplyToAave_selectorIsGone() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        (bool ok,) = address(bridge).call(abi.encodeWithSelector(0x70ded44d, uint256(1)));
        assertFalse(ok, "owner");
        vm.prank(user1);
        (ok,) = address(bridge).call(abi.encodeWithSelector(0x70ded44d, uint256(1)));
        assertFalse(ok, "anyone");
    }

    /// @dev DEP-5: with `aaveEnabled` false a crossing deposit makes no pool call.
    function test_deposit_whenDisabledDoesNotSupply() public {
        bridge.setAaveEnabled(false);
        vm.expectCall(address(pool), abi.encodeWithSelector(IAavePool.supply.selector), 0);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 0);
        assertEq(usdc.balanceOf(address(bridge)), 10 * UsdcTestLib.UNIT);
    }

    /// @dev DEP-4: a deposit below the threshold makes no pool call.
    function test_deposit_belowThresholdDoesNotSupply() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        // L = 1 + 1 = 2, B = 11, T = 1.1, H = 2.2.
        vm.expectCall(address(pool), abi.encodeWithSelector(IAavePool.supply.selector), 0);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 1 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 2 * UsdcTestLib.UNIT);
    }

    /// @dev DEP-8: USDC above the principal is not supplied.
    function test_deposit_doesNotSupplyUsdcAboveThePrincipal() public {
        usdc.mint(address(bridge), 100 * UsdcTestLib.UNIT);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 101 * UsdcTestLib.UNIT);
        assertLe(bridge.suppliedPrincipal(), bridge.treasuryBalance());
    }

    // -----------------------------------------------------------------
    // withdrawFromAave (owner preemptive top-up)
    // -----------------------------------------------------------------

    function test_withdrawFromAave_preemptivelyTopsUp() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);

        bridge.withdrawFromAave(3 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 6 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 4 * UsdcTestLib.UNIT);
    }

    function test_withdrawFromAave_onlyOwner() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.withdrawFromAave(1 * UsdcTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // Yield
    // -----------------------------------------------------------------

    function test_accruedYield_reflectsAaveGrowth() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);

        aUSDC.accrueYield(address(bridge), 500_000);

        assertEq(bridge.accruedYield(), 500_000, "accrued yield visible");
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT, "principal unchanged");
    }

    function test_harvestYield_sendsToRecipient() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);

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

        aUSDC.accrueYield(address(bridge), 400_000);

        bridge.setYieldRecipient(yieldSink);
        bridge.harvestYield(100_000);
        assertEq(bridge.accruedYield(), 300_000, "remaining yield");
        assertEq(usdc.balanceOf(yieldSink), 100_000, "recipient paid");
    }

    function test_harvestYield_amountExceedsYieldReverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);

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
    // Emergency
    // -----------------------------------------------------------------

    function test_emergencyWithdrawAll_pullsEverything() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);

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

    /// @notice DEP-7: book the aUSDC delta, not the USDC sent. A pool that
    ///         mints fewer shares than it pulls must not inflate
    ///         `suppliedPrincipal` above `aUsdcBalance`.
    function test_deposit_booksATokenDelta() public {
        uint256 haircut = 10;
        pool.setSupplyHaircut(haircut);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT - haircut);
        assertEq(bridge.aUsdcBalance(), bridge.suppliedPrincipal());
    }

    /// @notice After a full aToken drain, keep `principal - received` on the
    ///         books instead of zeroing. Harvest still sees no yield.
    function test_emergencyWithdrawAll_keepsShortfallOnBooks() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
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

    /// @dev DEP-6 / DEP-9: zero credited aUSDC — the deposit succeeds,
    ///      nothing is booked, and the refusal names the reason.
    function test_deposit_zeroDeltaIsReportedNotReverted() public {
        uint256 toSupply = 9 * UsdcTestLib.UNIT;
        pool.setSupplyHaircut(toSupply);
        _fund(user1, 10 * UsdcTestLib.UNIT);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveSupplyRefused(
            toSupply, abi.encodeWithSelector(AckiNackiBridge.AaveSupplyFailed.selector, toSupply, 0)
        );
        _depositFunded(user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 0);
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT);
        assertEq(bridge.depositCounter(), 1);
    }

    // -----------------------------------------------------------------
    // ETH-28: book above the aToken balance
    // -----------------------------------------------------------------

    /// @dev 10 deposited, 9 supplied, `lost` aUSDC gone (a socialized loss).
    function _supplyThenLose(uint256 lost) internal {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), lost);
    }

    function test_writeOffUnbackedPrincipal_revertsWhenBooksAreBacked() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.NothingToWriteOff.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_writeOffUnbackedPrincipal_revertsWhenBooksAreEmpty() public {
        vm.expectRevert(AckiNackiBridge.NothingToWriteOff.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_writeOffUnbackedPrincipal_onlyOwner() public {
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.writeOffUnbackedPrincipal();
    }

    function test_writeOffUnbackedPrincipal_clampsToDust() public {
        _supplyThenLose(9 * UsdcTestLib.UNIT - 1);

        vm.expectEmit(false, false, false, true);
        emit UnbackedPrincipalWrittenOff(9 * UsdcTestLib.UNIT - 1);
        bridge.writeOffUnbackedPrincipal();
        assertEq(bridge.suppliedPrincipal(), 1);
        assertEq(bridge.aUsdcBalance(), 1);
        assertEq(bridge.accruedYield(), 0);
    }

    function test_withdrawFromAave_maxPullsBackedWhenUnderBooked() public {
        uint256 lost = 10;
        _supplyThenLose(lost);

        bridge.withdrawFromAave(type(uint256).max);
        assertEq(bridge.aUsdcBalance(), 0);
        assertEq(bridge.suppliedPrincipal(), lost, "unbacked part stays booked");
    }

    function test_withdrawFromAave_aboveBackedReverts() public {
        _supplyThenLose(10);
        uint256 backed = bridge.aUsdcBalance();
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.withdrawFromAave(backed + 1);
    }

    /// @dev After a loss and a write-off, interest first refills what users
    ///      are owed; only the surplus over `treasuryBalance` is harvestable.
    function test_harvestYield_revertsWhenWouldBreakSolvency() public {
        uint256 lost = 2 * UsdcTestLib.UNIT;
        _supplyThenLose(lost);
        bridge.writeOffUnbackedPrincipal();
        assertEq(bridge.accruedYield(), 0);

        uint256 interest = 3 * UsdcTestLib.UNIT;
        aUSDC.accrueYield(address(bridge), interest);
        usdc.mint(address(pool), interest);
        assertEq(bridge.accruedYield(), interest, "book-side yield");
        bridge.setYieldRecipient(yieldSink);

        uint256 backing = usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance();
        uint256 treasury = bridge.treasuryBalance();
        assertEq(backing, treasury + interest - lost);

        uint256 surplus = interest - lost;
        // `harvestYield(accruedYield())` after a write-off pays only the
        // surplus over treasury — the operator does not have to guess.
        bridge.harvestYield(interest);
        assertEq(usdc.balanceOf(yieldSink), surplus);
        assertEq(usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance(), treasury);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HarvestWouldBreakSolvency.selector, 1, treasury, treasury
            )
        );
        bridge.harvestYield(1);
    }

    /// @dev AAVE minted 1 wei less than the USDC it took. `accruedYield()`
    ///      still reports the full interest; the solvency check used to
    ///      revert `HarvestWouldBreakSolvency` for that 1 wei.
    function test_harvestYield_accruedYieldSafeAfterAaveRounding() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
        deal(address(usdc), address(bridge), usdc.balanceOf(address(bridge)) - 1);

        uint256 interest = 500_000;
        aUSDC.accrueYield(address(bridge), interest);
        usdc.mint(address(pool), interest);
        assertEq(bridge.accruedYield(), interest);

        bridge.setYieldRecipient(yieldSink);
        bridge.harvestYield(interest);
        assertEq(usdc.balanceOf(yieldSink), interest - 1);
        assertEq(usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance(), bridge.treasuryBalance());
    }

    /// @dev DEP-6 / DEP-9: `approve` false — the deposit succeeds, its USDC
    ///      stays, nothing is booked.
    function test_eth9_approveFalse_depositKeepsUsdc() public {
        _fund(user1, 10 * UsdcTestLib.UNIT);
        usdc.setApproveReturnsFalse(true);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveSupplyRefused(
            9 * UsdcTestLib.UNIT, abi.encodeWithSelector(AckiNackiBridge.ApproveFailed.selector)
        );
        _depositFunded(user1, 10 * UsdcTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 0, "no principal booked on failed approve");
        assertEq(usdc.balanceOf(address(bridge)), 10 * UsdcTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------

    /// @dev CFG-3, CFG-6, CFG-10.
    function test_setLiquidReserveBps_capped() public {
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooHigh.selector);
        bridge.setLiquidReserveBps(5_001);
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooLow.selector);
        bridge.setLiquidReserveBps(999);
        bridge.setLiquidReserveBps(2_500);
        assertEq(bridge.liquidReserveBps(), 2_500);
        bridge.setLiquidReserveBps(5_000);
        bridge.setSupplyMultiplierBps(15_000);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.ReserveBandTooWide.selector, 5_000, 30_000)
        );
        bridge.setSupplyMultiplierBps(30_000);
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
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setSupplyMultiplierBps(30_000);
        vm.startPrank(user2);
        bridge.setAaveEnabled(false);
        bridge.setSupplyMultiplierBps(30_000);
        vm.stopPrank();
        assertEq(bridge.supplyMultiplierBps(), 30_000);
    }

    function test_acceptOwnership_movesDefaultYieldRecipient_harvestPaysNewOwner() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, 10 * UsdcTestLib.UNIT);
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

    /// @dev INV-1 over a few deposits with `r` and `M` anywhere in range.
    function testFuzz_totalAssetsCoversTreasury(
        uint96 depositAmt,
        uint16 bps,
        uint32 multiplier,
        uint8 count
    ) public {
        depositAmt = uint96(bound(depositAmt, 1, bridge.MAX_TRANSFER_AMOUNT()));
        bps = uint16(bound(bps, bridge.MIN_LIQUID_RESERVE_BPS(), bridge.MAX_LIQUID_RESERVE_BPS()));
        uint256 maxMultiplier = (bridge.BPS_DENOMINATOR() * bridge.BPS_DENOMINATOR()) / bps;
        multiplier = uint32(bound(multiplier, bridge.BPS_DENOMINATOR() + 1, maxMultiplier));
        count = uint8(bound(count, 1, 8));

        bridge.setLiquidReserveBps(bps);
        bridge.setSupplyMultiplierBps(multiplier);
        for (uint256 i; i < count; ++i) {
            UsdcTestLib.depositUsdc(vm, usdc, bridge, user1, depositAmt);
            assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "solvent");
            assertLe(bridge.suppliedPrincipal(), bridge.treasuryBalance(), "S <= B");
        }
    }

    function testFuzz_depositAboveTransferLimitLeavesCustodyUnchanged(uint256 amount) public {
        amount = bound(amount, 50 * UsdcTestLib.UNIT + 1, type(uint64).max);
        usdc.mint(user1, amount);
        vm.startPrank(user1);
        usdc.approve(address(bridge), amount);
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.deposit(address(usdc), amount, bytes32(uint256(1)));
        vm.stopPrank();
        assertEq(usdc.balanceOf(user1), amount);
        assertEq(usdc.balanceOf(address(bridge)), 0);
        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);
    }

    function test_tvlCapIncludesPrincipalSuppliedToAave() public {
        usdc.mint(user1, 10_000 * UsdcTestLib.UNIT + 1);
        vm.startPrank(user1);
        usdc.approve(address(bridge), type(uint256).max);
        for (uint256 i; i < 200; ++i) {
            bridge.deposit(address(usdc), 50 * UsdcTestLib.UNIT, bytes32(uint256(1)));
        }
        vm.stopPrank();
        assertEq(bridge.treasuryBalance(), 10_000 * UsdcTestLib.UNIT);
        assertGt(bridge.suppliedPrincipal(), 0);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.TvlCapReached.selector);
        bridge.deposit(address(usdc), 1, bytes32(uint256(1)));
        assertEq(usdc.balanceOf(user1), 1);
        assertEq(bridge.depositCounter(), 200);
        assertEq(bridge.treasuryBalance(), 10_000 * UsdcTestLib.UNIT);
    }

    /// @dev Mint and approve, so that the next call is the deposit itself.
    function _fund(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(address(bridge), amount);
    }

    function _depositFunded(address who, uint256 amount) internal {
        vm.prank(who);
        bridge.deposit(address(usdc), amount, bytes32(uint256(uint160(who))));
    }
}
