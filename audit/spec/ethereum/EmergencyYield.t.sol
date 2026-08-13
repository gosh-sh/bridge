// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title EmergencyYieldTest
/// @notice Phase C / A1 — QC-A1-3 / A1-F5: yield trapped after emergencyWithdrawAll.
/// @dev INV: TR-3 — accrued yield accounting vs harvest path
contract EmergencyYieldTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal user = address(0xA1);
    address internal yieldSink = address(0xBEEF);

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

    /// @dev QC-A1-3: emergency pulls yield as liquid USDC but harvestYield path is dead.
    function test_emergencyWithdrawAll_yieldNotHarvestable() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);

        aUSDC.accrueYield(address(bridge), 500_000);
        usdc.mint(address(pool), 500_000);

        assertEq(bridge.accruedYield(), 500_000, "pre: yield visible on aUSDC");
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT, "pre: principal ledger unchanged");

        bridge.emergencyWithdrawAll();

        assertEq(bridge.suppliedPrincipal(), 0, "principal book cleared");
        assertEq(bridge.aUsdcBalance(), 0, "aUSDC drained");
        assertEq(bridge.accruedYield(), 0, "post: no harvestable yield metric");
        assertEq(usdc.balanceOf(address(bridge)), 10_500_000, "liquid includes yield");
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT, "treasury ledger unchanged");

        vm.expectRevert(AckiNackiBridge.NoYield.selector);
        bridge.harvestYield(1);

        assertEq(usdc.balanceOf(yieldSink), 0, "yield sink untouched");
    }
}
