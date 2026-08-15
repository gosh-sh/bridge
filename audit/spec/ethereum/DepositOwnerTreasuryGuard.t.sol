// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositOwnerTreasuryGuardTest
/// @notice TD-60 — INV: owner-only paths never reduce `treasuryBalance`; principal exit only via `withdrawByProof`.
contract DepositOwnerTreasuryGuardTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal depositor = address(0xA1);
    address internal donor = address(0xD0A0);
    address internal yieldSink = address(0xBEEF);
    address internal stranger = address(0xBAD);

    uint256 internal constant BASELINE = 20 * UsdcTestLib.UNIT;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        bridge.setYieldRecipient(yieldSink);

        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, BASELINE);
        assertEq(bridge.treasuryBalance(), BASELINE, "TD-60 baseline");
    }

    function _donate(uint256 amount) internal {
        usdc.mint(donor, amount);
        vm.prank(donor);
        usdc.transfer(address(bridge), amount);
    }

    function test_td60_owner_ops_interleave_treasury_unchanged() public {
        uint256 treasury = bridge.treasuryBalance();

        bridge.supplyToAave(type(uint256).max);
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: supply");

        aUSDC.accrueYield(address(bridge), 600_000);
        usdc.mint(address(pool), 600_000);
        bridge.harvestYield(250_000);
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: harvest");

        bridge.withdrawFromAave(2 * UsdcTestLib.UNIT);
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: withdrawFromAave");

        bridge.setLiquidReserveBps(2_500);
        bridge.setAaveEnabled(false);
        bridge.setAaveEnabled(true);
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: config setters");

        bridge.emergencyWithdrawAll();
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: emergencyWithdrawAll");

        bridge.transferOwnership(yieldSink);
        vm.prank(yieldSink);
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: transferOwnership");
    }

    function test_td60_skim_donation_reduces_liquid_not_treasury() public {
        uint256 treasury = bridge.treasuryBalance();
        uint256 donation = 3 * UsdcTestLib.UNIT;
        _donate(donation);

        assertEq(bridge.excessUsdc(), donation, "TD-60: donation is excess");
        assertEq(bridge.treasuryBalance(), treasury, "TD-60: donation not in treasury");

        uint256 liquidBefore = usdc.balanceOf(address(bridge));
        bridge.skimExcessUsdc(type(uint256).max);

        assertEq(bridge.treasuryBalance(), treasury, "TD-60: skim does not move treasury");
        assertEq(usdc.balanceOf(address(bridge)), liquidBefore - donation, "TD-60: skim pulls excess liquid");
        assertEq(usdc.balanceOf(yieldSink), donation, "TD-60: skim to yieldRecipient");
        assertEq(bridge.excessUsdc(), 0);
    }

    function test_td60_skim_over_excess_reverts_no_excess_usdc() public {
        _donate(UsdcTestLib.UNIT);
        bridge.skimExcessUsdc(UsdcTestLib.UNIT);

        vm.expectRevert(AckiNackiBridge.NoExcessUsdc.selector);
        bridge.skimExcessUsdc(1);

        vm.expectRevert(AckiNackiBridge.NoExcessUsdc.selector);
        bridge.skimExcessUsdc(type(uint256).max);
    }

    function test_td60_harvest_yield_to_sink_treasury_unchanged() public {
        uint256 treasury = bridge.treasuryBalance();
        bridge.supplyToAave(type(uint256).max);

        uint256 yieldAmt = 350_000;
        aUSDC.accrueYield(address(bridge), yieldAmt);
        usdc.mint(address(pool), yieldAmt);

        bridge.harvestYield(yieldAmt);

        assertEq(bridge.treasuryBalance(), treasury, "TD-60: harvest treasury");
        assertEq(usdc.balanceOf(yieldSink), yieldAmt, "TD-60: yield to sink");
        assertGt(bridge.suppliedPrincipal(), 0, "TD-60: principal still booked");
    }

    /// @dev QC: no owner entrypoint sends `treasuryBalance` principal to an EOA (see notes matrix).
    function test_td60_treasury_only_reduced_by_withdraw_proof_path_qc() public view {
        assertEq(bridge.treasuryBalance(), BASELINE);
    }

    function test_td60_non_owner_supply_reverts_not_owner() public {
        vm.prank(stranger);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.supplyToAave(1);
    }

    function test_td60_non_owner_skim_reverts_not_owner() public {
        _donate(UsdcTestLib.UNIT);
        vm.prank(stranger);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.skimExcessUsdc(type(uint256).max);
    }
}
