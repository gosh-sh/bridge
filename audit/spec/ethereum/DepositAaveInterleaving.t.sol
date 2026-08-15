// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositAaveInterleavingTest
/// @notice TD-66 — META: interleaved deposit / donation / `supplyToAave` / `harvestYield`
///         paths; `treasuryBalance` never decreases; `_amountSupplyable` matches reserve math.
contract DepositAaveInterleavingTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal depositor = address(0xA1);
    address internal depositor2 = address(0xA2);
    address internal donor = address(0xD0A0);
    address internal yieldSink = address(0xBEEF);

    uint256 internal constant D1 = 10 * UsdcTestLib.UNIT;
    uint256 internal constant D2 = 5 * UsdcTestLib.UNIT;
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

    function _reserveFloor() internal view returns (uint256) {
        return (bridge.treasuryBalance() * bridge.liquidReserveBps()) / bridge.BPS_DENOMINATOR();
    }

    /// Mirrors `AckiNackiBridge._amountSupplyable()` (internal view).
    function _amountSupplyableManual() internal view returns (uint256) {
        uint256 reserve = _reserveFloor();
        uint256 bal = usdc.balanceOf(address(bridge));
        if (bal <= reserve) return 0;
        return bal - reserve;
    }

    function _deposit(address user, uint256 amount) internal {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, amount);
    }

    function _assertTreasuryNeverBelow(uint256 floor, string memory label) internal view {
        assertGe(bridge.treasuryBalance(), floor, label);
    }

  /// (a) D1 → donate → D2 → supply(max): ledger = D1+D2; principal includes donation; liquid = reserve.
    function test_td66_a_deposit_donate_deposit_supply_max() public {
        uint256 treasuryFloor = 0;

        _deposit(depositor, D1);
        treasuryFloor = bridge.treasuryBalance();
        _assertTreasuryNeverBelow(treasuryFloor, "TD-66(a): after D1");

        _donate(DONATION);
        _assertTreasuryNeverBelow(treasuryFloor, "TD-66(a): after donate");

        _deposit(depositor2, D2);
        treasuryFloor = D1 + D2;
        assertEq(bridge.treasuryBalance(), treasuryFloor, "TD-66(a): ledger D1+D2");
        _assertTreasuryNeverBelow(treasuryFloor, "TD-66(a): after D2");

        uint256 reserve = _reserveFloor();
        uint256 liquidBefore = usdc.balanceOf(address(bridge));
        uint256 expectedSupply = liquidBefore - reserve;

        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.treasuryBalance(), treasuryFloor, "TD-66(a): treasury after supply");
        assertEq(bridge.suppliedPrincipal(), expectedSupply, "TD-66(a): principal includes donation excess");
        assertEq(usdc.balanceOf(address(bridge)), reserve, "TD-66(a): liquid at reserve floor");
        assertEq(bridge.suppliedPrincipal(), liquidBefore - reserve, "TD-66(a): supplied = pre-liquid - reserve");
    }

  /// (b) deposit → partial supply → second deposit → manual `_amountSupplyable` before supply.
    function test_td66_b_partial_supply_second_deposit_amount_supplyable() public {
        _deposit(depositor, D1);

        uint256 partialSupply = 3 * UsdcTestLib.UNIT;
        bridge.supplyToAave(partialSupply);
        assertEq(bridge.suppliedPrincipal(), partialSupply);

        _deposit(depositor2, D2);

        uint256 manual = _amountSupplyableManual();
        assertEq(manual, usdc.balanceOf(address(bridge)) - _reserveFloor(), "TD-66(b): manual formula");
        assertGt(manual, 0, "TD-66(b): supplyable > 0 before supply");

        bridge.supplyToAave(manual);
        assertEq(usdc.balanceOf(address(bridge)), _reserveFloor(), "TD-66(b): post-supply liquid floor");
        assertEq(bridge.treasuryBalance(), D1 + D2, "TD-66(b): treasury unchanged by AAVE");
    }

  /// (c) donate → deposit → harvest yield: treasury unchanged; yield to `yieldRecipient` only.
    function test_td66_c_donate_deposit_harvest_yield_to_sink() public {
        _donate(DONATION);
        _deposit(depositor, D1);

        uint256 treasuryLedger = bridge.treasuryBalance();
        bridge.supplyToAave(type(uint256).max);

        uint256 yieldAmt = 400_000;
        aUSDC.accrueYield(address(bridge), yieldAmt);
        usdc.mint(address(pool), yieldAmt);

        assertEq(bridge.accruedYield(), yieldAmt, "TD-66(c): yield accrued");
        bridge.harvestYield(yieldAmt);

        assertEq(bridge.treasuryBalance(), treasuryLedger, "TD-66(c): harvest does not move treasury");
        assertEq(usdc.balanceOf(yieldSink), yieldAmt, "TD-66(c): yield to sink only");
        assertEq(bridge.accruedYield(), 0, "TD-66(c): yield book cleared");
    }

  /// (d) Liquid exactly at reserve → `_amountSupplyable()==0`; further supply reverts.
    function test_td66_d_liquid_at_reserve_supply_noop() public {
        _deposit(depositor, D1);

        uint256 reserve = _reserveFloor();
        uint256 supplyable = _amountSupplyableManual();
        bridge.supplyToAave(supplyable);

        assertEq(usdc.balanceOf(address(bridge)), reserve, "TD-66(d): liquid == reserve");
        assertEq(_amountSupplyableManual(), 0, "TD-66(d): nothing supplyable");

        vm.expectRevert(AckiNackiBridge.NothingToSupply.selector);
        bridge.supplyToAave(type(uint256).max);
    }

  /// INV: `treasuryBalance` never decreases across deposit / donation / AAVE ops.
    function test_td66_inv_treasury_never_decreases_on_deposit_aave_path() public {
        uint256 treasuryMin = 0;

        _deposit(depositor, D1);
        treasuryMin = bridge.treasuryBalance();
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post deposit");

        _donate(DONATION);
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post donate");

        bridge.supplyToAave(2 * UsdcTestLib.UNIT);
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post partial supply");

        _deposit(depositor2, D2);
        treasuryMin = bridge.treasuryBalance();
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post D2");

        bridge.supplyToAave(type(uint256).max);
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post max supply");

        aUSDC.accrueYield(address(bridge), 200_000);
        usdc.mint(address(pool), 200_000);
        bridge.harvestYield(200_000);
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post harvest");

        _donate(UsdcTestLib.UNIT);
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post second donate");

        if (bridge.excessUsdc() > 0) {
            bridge.skimExcessUsdc(type(uint256).max);
        }
        _assertTreasuryNeverBelow(treasuryMin, "TD-66 INV: post skim");
    }
}
