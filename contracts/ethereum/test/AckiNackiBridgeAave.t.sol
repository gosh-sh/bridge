// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./mocks/MockAave.sol";
import "./mocks/MockERC20.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdtTestLib.sol";

/// @title AckiNackiBridgeAaveTest
/// @notice Exercises the AAVE USDT integration surface of AckiNackiBridge.
contract AckiNackiBridgeAaveTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;

    MockERC20 internal usdt;
    MockAUSDT internal aUSDT;
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

        usdt = new MockERC20("Mock USDT", "mUSDT", 6);
        aUSDT = new MockAUSDT();
        pool = new MockAavePool(address(usdt), address(aUSDT));

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdt),
            address(pool),
            address(aUSDT),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    // -----------------------------------------------------------------
    // Constructor / config
    // -----------------------------------------------------------------

    function test_constructor_wiresAaveAndUsdt() public view {
        assertEq(address(bridge.usdt()), address(usdt), "usdt wired");
        assertEq(address(bridge.aavePool()), address(pool), "pool wired");
        assertEq(address(bridge.aUSDT()), address(aUSDT), "aUSDT wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default when wired");
        assertEq(bridge.owner(), owner, "owner");
        assertEq(bridge.yieldRecipient(), owner, "yield recipient defaults to owner");
        assertEq(bridge.liquidReserveBps(), 1_000, "default 10% reserve");
        assertEq(bridge.MAX_DEPOSIT_AMOUNT(), 100 * UsdtTestLib.UNIT, "100 USDT cap");
    }

    function test_constructor_partialAaveWiringReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidAaveAddress.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdt),
            address(pool),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_constructor_zeroUsdtReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidUsdt.selector);
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
            address(usdt),
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
        uint256 depositAmount = 10 * UsdtTestLib.UNIT;
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, depositAmount);

        uint256 reserve = 1 * UsdtTestLib.UNIT;
        uint256 supplyable = 9 * UsdtTestLib.UNIT;
        vm.expectEmit(false, false, false, true);
        emit SuppliedToAave(supplyable, supplyable);
        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.suppliedPrincipal(), supplyable, "principal");
        assertEq(bridge.aUsdtBalance(), supplyable, "aUSDT balance");
        assertEq(usdt.balanceOf(address(bridge)), reserve, "liquid reserve kept");
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury unchanged");
    }

    function test_supplyToAave_explicitAmount() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(5 * UsdtTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 5 * UsdtTestLib.UNIT);
        assertEq(usdt.balanceOf(address(bridge)), 5 * UsdtTestLib.UNIT);
    }

    function test_supplyToAave_onlyOwner() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_whenDisabledReverts() public {
        bridge.setAaveEnabled(false);
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.AaveDisabled.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_nothingToSupplyReverts() public {
        vm.expectRevert(AckiNackiBridge.NothingToSupply.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_amountExceedsAvailableReverts() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.supplyToAave(10 * UsdtTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // withdrawFromAave (owner preemptive top-up)
    // -----------------------------------------------------------------

    function test_withdrawFromAave_preemptivelyTopsUp() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        bridge.withdrawFromAave(3 * UsdtTestLib.UNIT);
        assertEq(bridge.suppliedPrincipal(), 6 * UsdtTestLib.UNIT);
        assertEq(usdt.balanceOf(address(bridge)), 4 * UsdtTestLib.UNIT);
    }

    function test_withdrawFromAave_onlyOwner() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.withdrawFromAave(1 * UsdtTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // Yield
    // -----------------------------------------------------------------

    function test_accruedYield_reflectsAaveGrowth() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDT.accrueYield(address(bridge), 500_000);

        assertEq(bridge.accruedYield(), 500_000, "accrued yield visible");
        assertEq(bridge.suppliedPrincipal(), 9 * UsdtTestLib.UNIT, "principal unchanged");
    }

    function test_harvestYield_sendsToRecipient() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDT.accrueYield(address(bridge), 300_000);

        bridge.setYieldRecipient(yieldSink);

        uint256 sinkBefore = usdt.balanceOf(yieldSink);
        vm.expectEmit(true, false, false, true);
        emit YieldHarvested(yieldSink, 300_000);
        bridge.harvestYield(300_000);

        assertEq(usdt.balanceOf(yieldSink), sinkBefore + 300_000, "yield paid");
        assertEq(bridge.suppliedPrincipal(), 9 * UsdtTestLib.UNIT, "principal intact");
        assertEq(bridge.accruedYield(), 0, "yield consumed");
    }

    function test_harvestYield_partialHarvest() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDT.accrueYield(address(bridge), 400_000);

        bridge.setYieldRecipient(yieldSink);
        bridge.harvestYield(100_000);
        assertEq(bridge.accruedYield(), 300_000, "remaining yield");
        assertEq(usdt.balanceOf(yieldSink), 100_000, "recipient paid");
    }

    function test_harvestYield_amountExceedsYieldReverts() public {
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDT.accrueYield(address(bridge), 100_000);

        vm.expectRevert(AckiNackiBridge.NoYield.selector);
        bridge.harvestYield(1 * UsdtTestLib.UNIT);
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
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDT.accrueYield(address(bridge), 700_000);
        // Back the synthetic yield with underlying USDT in the pool.
        usdt.mint(address(pool), 700_000);

        vm.expectEmit(false, false, false, true);
        emit AaveEnabledSet(false);
        bridge.emergencyWithdrawAll();

        assertFalse(bridge.aaveEnabled(), "aave disabled");
        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        assertEq(bridge.aUsdtBalance(), 0, "aUSDT drained");
        assertEq(usdt.balanceOf(address(bridge)), 10_700_000, "all USDT home");
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
        vm.assume(depositAmt > 0 && depositAmt <= 100 * UsdtTestLib.UNIT);
        vm.assume(bps <= bridge.MAX_LIQUID_RESERVE_BPS());

        bridge.setLiquidReserveBps(bps);
        UsdtTestLib.depositUsdt(vm, usdt, bridge, user1, depositAmt);

        uint256 reserve = (uint256(depositAmt) * bps) / bridge.BPS_DENOMINATOR();
        uint256 supplyable = depositAmt > reserve ? uint256(depositAmt) - reserve : 0;
        if (supplyable > 0) {
            bridge.supplyToAave(supplyable);
        }

        assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "solvent");
    }
}
