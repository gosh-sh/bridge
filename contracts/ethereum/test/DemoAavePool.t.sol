// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/DemoAavePool.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./mocks/MockERC20.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/VerifyBlockConfigLib.sol";

contract DemoAavePoolTest is Test {
    MockERC20 internal usdc;
    DemoAavePool internal pool;
    DemoAToken internal aToken;
    AckiNackiBridge internal bridge;

    address internal poolOwner = address(0x0A11);
    address internal funder = address(0xF00D);
    address internal yieldSink = address(0xBEEF);

    uint256 internal constant UNIT = UsdcTestLib.UNIT;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        pool = new DemoAavePool(address(usdc), poolOwner);
        aToken = pool.aToken();
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aToken),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        bridge.setYieldRecipient(yieldSink);

        usdc.mint(poolOwner, 1_000 * UNIT);
        vm.prank(poolOwner);
        usdc.approve(address(pool), type(uint256).max);
    }

    function _assertFullyBacked() internal view {
        assertEq(usdc.balanceOf(address(pool)), aToken.totalSupply(), "pool fully backed");
    }

    // ── pool on its own ───────────────────────────────────────────────────

    function test_reserveData_listsOnlyTheUnderlying() public view {
        IAavePool.ReserveData memory r = pool.getReserveData(address(usdc));
        assertEq(r.aTokenAddress, address(aToken));
        assertEq(r.configuration.data, 0, "no supply cap");
        assertEq(pool.getReserveData(address(0xDEAD)).aTokenAddress, address(0));
    }

    function test_supplyAndWithdraw_areOneToOne() public {
        usdc.mint(address(this), 10 * UNIT);
        usdc.approve(address(pool), 10 * UNIT);
        pool.supply(address(usdc), 10 * UNIT, address(this), 0);
        assertEq(aToken.balanceOf(address(this)), 10 * UNIT);

        assertEq(pool.withdraw(address(usdc), 4 * UNIT, address(0xCAFE)), 4 * UNIT);
        assertEq(usdc.balanceOf(address(0xCAFE)), 4 * UNIT);
        assertEq(pool.withdraw(address(usdc), type(uint256).max, address(this)), 6 * UNIT);
        assertEq(aToken.totalSupply(), 0);
        _assertFullyBacked();
    }

    function test_withdraw_moreThanBalance_reverts() public {
        vm.expectRevert(DemoAavePool.InvalidAmount.selector);
        pool.withdraw(address(usdc), 1, address(this));
    }

    function test_otherAsset_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(DemoAavePool.UnsupportedAsset.selector, address(1)));
        pool.supply(address(1), 1, address(this), 0);
        vm.expectRevert(abi.encodeWithSelector(DemoAavePool.UnsupportedAsset.selector, address(1)));
        pool.withdraw(address(1), 1, address(this));
    }

    function test_accrueYield_onlyOwner_andBacked() public {
        vm.expectRevert(DemoAavePool.NotOwner.selector);
        pool.accrueYield(address(bridge), 1 * UNIT);

        vm.prank(poolOwner);
        pool.accrueYield(address(bridge), 3 * UNIT);
        assertEq(aToken.balanceOf(address(bridge)), 3 * UNIT);
        assertEq(usdc.balanceOf(poolOwner), 997 * UNIT, "yield is paid in, not printed");
        _assertFullyBacked();
    }

    function test_aToken_onlyPoolMintsAndBurns() public {
        vm.expectRevert(DemoAToken.OnlyPool.selector);
        aToken.mint(address(this), 1);
        vm.expectRevert(DemoAToken.OnlyPool.selector);
        aToken.burn(address(this), 0);
    }

    // ── the bridge on top of it ───────────────────────────────────────────

    function test_bridge_supplyAccrueHarvest() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 100 * UNIT);
        bridge.supplyToAave(type(uint256).max);
        assertEq(bridge.suppliedPrincipal(), 90 * UNIT, "10% stays liquid");

        vm.prank(poolOwner);
        pool.accrueYield(address(bridge), 5 * UNIT);
        assertEq(bridge.accruedYield(), 5 * UNIT);

        bridge.harvestYield(bridge.accruedYield());
        assertEq(usdc.balanceOf(yieldSink), 5 * UNIT, "yield reached the recipient");
        assertEq(bridge.suppliedPrincipal(), 90 * UNIT, "principal untouched");
        assertEq(bridge.totalAssets(), bridge.treasuryBalance(), "users fully backed");
        _assertFullyBacked();
    }

    function test_bridge_emergencyUnwind_thenSkim() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 100 * UNIT);
        bridge.supplyToAave(type(uint256).max);
        vm.prank(poolOwner);
        pool.accrueYield(address(bridge), 2 * UNIT);

        bridge.emergencyWithdrawAll();
        assertEq(bridge.suppliedPrincipal(), 0);
        assertFalse(bridge.aaveEnabled());
        assertEq(bridge.excessUsdc(), 2 * UNIT, "yield came back liquid");

        bridge.skimExcessUsdc(type(uint256).max);
        assertEq(usdc.balanceOf(yieldSink), 2 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), bridge.treasuryBalance());
    }
}
