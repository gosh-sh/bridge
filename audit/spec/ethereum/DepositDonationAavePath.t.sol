// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositDonationAavePathTest
/// @notice TD-35 — TR-3 donation vs `treasuryBalance`; donate → AAVE → skim path.
/// @dev QC-A1-3 / TR-1: donation is liquid excess, not ledger principal.
contract DepositDonationAavePathTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal depositor = address(0xA1);
    address internal donor = address(0xD0A0);
    address internal yieldSink = address(0xBEEF);

    uint256 internal constant DEPOSIT = 10 * UsdcTestLib.UNIT;
    uint256 internal constant DONATION = 2 * UsdcTestLib.UNIT;

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
        bridge.setYieldRecipient(yieldSink);
    }

    function _donate(uint256 amount) internal {
        usdc.mint(donor, amount);
        vm.prank(donor);
        usdc.transfer(address(bridge), amount);
    }

    function test_td35_a_deposit_credits_treasury_donation_only_liquid() public {
        uint256 treasury0 = bridge.treasuryBalance();
        uint256 liquid0 = usdc.balanceOf(address(bridge));

        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, DEPOSIT);
        assertEq(bridge.treasuryBalance(), treasury0 + DEPOSIT, "deposit credits ledger");
        assertEq(usdc.balanceOf(address(bridge)), liquid0 + DEPOSIT, "deposit custody");

        _donate(DONATION);
        assertEq(bridge.treasuryBalance(), treasury0 + DEPOSIT, "TR-3: donation not in treasuryBalance");
        assertEq(usdc.balanceOf(address(bridge)), liquid0 + DEPOSIT + DONATION, "donation increases liquid");
        assertEq(bridge.excessUsdc(), DONATION, "donation is excess above ledger");
    }

    function test_td35_b_supply_after_donation_moves_donation_into_aave_principal() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, DEPOSIT);
        _donate(DONATION);

        uint256 reserve = (bridge.treasuryBalance() * bridge.liquidReserveBps()) / bridge.BPS_DENOMINATOR();
        uint256 expectedSupply = DEPOSIT + DONATION - reserve;

        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.suppliedPrincipal(), expectedSupply, "donated USDC can enter AAVE principal");
        assertEq(bridge.treasuryBalance(), DEPOSIT, "ledger still deposit-only");
        assertEq(usdc.balanceOf(address(bridge)), reserve, "liquid held at reserve floor");
    }

    function test_td35_c_skim_excess_donation_not_withdrawable_principal() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, DEPOSIT);
        _donate(DONATION);

        assertEq(bridge.excessUsdc(), DONATION);
        bridge.skimExcessUsdc(type(uint256).max);

        assertEq(bridge.treasuryBalance(), DEPOSIT, "skim does not credit treasuryBalance");
        assertEq(usdc.balanceOf(yieldSink), DONATION, "donation skimmed to yieldRecipient");
        assertEq(bridge.excessUsdc(), 0);
        assertEq(usdc.balanceOf(address(bridge)), DEPOSIT, "remaining liquid = deposit ledger");

        // Accrue on principal path — yield harvest does not inflate treasury ledger.
        bridge.supplyToAave(type(uint256).max);
        aUSDC.accrueYield(address(bridge), 300_000);
        usdc.mint(address(pool), 300_000);
        bridge.harvestYield(300_000);

        assertEq(bridge.treasuryBalance(), DEPOSIT, "harvested yield not in treasuryBalance");
        assertEq(usdc.balanceOf(yieldSink), DONATION + 300_000, "yield to sink, not user principal");
    }

    function test_td35_d_full_donate_aave_accrue_skim_tr1_solvency() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, DEPOSIT);
        uint256 treasuryLedger = bridge.treasuryBalance();

        _donate(DONATION);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 250_000);
        usdc.mint(address(pool), 250_000);
        bridge.harvestYield(250_000);

        _donate(UsdcTestLib.UNIT);
        if (bridge.excessUsdc() > 0) {
            bridge.skimExcessUsdc(type(uint256).max);
        }

        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 principal = bridge.suppliedPrincipal();
        assertGe(liquid + principal, bridge.treasuryBalance(), "TR-1 after donate/AAVE/skim path");
        assertEq(bridge.treasuryBalance(), treasuryLedger, "treasury ledger = deposits only");
    }
}
