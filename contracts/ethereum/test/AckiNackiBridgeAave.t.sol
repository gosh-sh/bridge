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
        assertEq(bridge.MAX_DEPOSIT_AMOUNT(), 100 * UsdcTestLib.UNIT, "100 USDC cap");
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
        bridge.transferOwnership(user2);
        assertEq(bridge.owner(), user2);

        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setAaveEnabled(false);
        vm.prank(user2);
        bridge.setAaveEnabled(false);
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
