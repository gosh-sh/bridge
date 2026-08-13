// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title HarvestYieldBoundTest
/// @notice Phase C / A4 — A4-INV-2: harvest bounded by accruedYield; principal intact.
contract HarvestYieldBoundTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal user = address(0xA1);
    address internal yieldSink = address(0xBEEF);

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
    }

    function test_harvestYield_exactAccrued_succeeds() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 principalAfterSupply = bridge.suppliedPrincipal();

        aUSDC.accrueYield(address(bridge), 250_000);
        uint256 yield = bridge.accruedYield();

        bridge.harvestYield(yield);

        assertEq(usdc.balanceOf(yieldSink), 250_000, "yield paid out");
        assertEq(bridge.suppliedPrincipal(), principalAfterSupply, "principal book intact");
        assertEq(bridge.accruedYield(), 0, "yield exhausted");
    }

    function test_harvestYield_oneWeiOverAccrued_reverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        aUSDC.accrueYield(address(bridge), 100_000);

        uint256 yield = bridge.accruedYield();
        assertGt(yield, 0, "precondition");

        vm.expectRevert(AckiNackiBridge.NoYield.selector);
        bridge.harvestYield(yield + 1);
    }
}
