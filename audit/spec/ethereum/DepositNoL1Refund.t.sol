// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositNoL1RefundTest
/// @notice TD-34 — Phase 4.3 retired legacy deposit `withdraw()`; stuck USDC has no user L1 refund.
/// @dev QC: recovery is AN mint or owner `emergencyWithdrawAll` (custody move, not per-depositor).
contract DepositNoL1RefundTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal owner = address(this);
    address internal depositor = address(0xDE11);
    address internal stranger = address(0xBAD);

    uint256 internal constant AMOUNT = 5 * UsdcTestLib.UNIT;

    /// Retired Phase 4.3 refund entrypoint (see `AckiNackiBridge.sol` header).
    bytes4 internal constant LEGACY_WITHDRAW_SELECTOR =
        bytes4(keccak256("withdraw(uint256,address,uint256,uint256,bytes)"));

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

    function test_td34_deposit_increases_treasury_and_custody() public {
        uint256 treasuryBefore = bridge.treasuryBalance();
        uint256 custodyBefore = usdc.balanceOf(address(bridge));

        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, AMOUNT);

        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), treasuryBefore + AMOUNT);
        assertEq(usdc.balanceOf(address(bridge)), custodyBefore + AMOUNT);
        assertEq(usdc.balanceOf(depositor), 0, "user USDC moved to bridge");
    }

    function test_td34_legacy_user_withdraw_selector_not_callable() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, AMOUNT);

        (bool ok, ) = address(bridge).call(
            abi.encodeWithSelector(
                LEGACY_WITHDRAW_SELECTOR,
                0,
                depositor,
                AMOUNT,
                block.number,
                hex"00"
            )
        );
        assertFalse(ok, "TD-34: legacy deposit withdraw must not exist");
        assertEq(usdc.balanceOf(depositor), 0, "no refund path");
        assertEq(bridge.treasuryBalance(), AMOUNT, "funds remain locked in treasury ledger");
    }

    function test_td34_non_owner_emergencyWithdrawAll_reverts() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, AMOUNT);
        bridge.supplyToAave(type(uint256).max);

        vm.prank(stranger);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.emergencyWithdrawAll();
    }

    function test_td34_owner_emergency_pulls_custody_not_user_selective_refund() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, AMOUNT);
        bridge.supplyToAave(type(uint256).max);

        uint256 depositorBefore = usdc.balanceOf(depositor);
        bridge.emergencyWithdrawAll();

        assertEq(usdc.balanceOf(depositor), depositorBefore, "TD-34: depositor not paid out");
        assertEq(bridge.treasuryBalance(), AMOUNT, "treasury ledger unchanged");
        assertGe(usdc.balanceOf(address(bridge)), AMOUNT, "USDC on bridge, owner custody only");
    }

    /// Unprovable / unfinalized scenario: deposit succeeds; no user entrypoint reduces treasury.
    function test_td34_stuck_deposit_scenario_no_user_recovery_path() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, depositor, AMOUNT);

        // Simulate prove-fail / AN-reject ops: L1 state unchanged except custody lock.
        assertEq(bridge.treasuryBalance(), AMOUNT);

        vm.startPrank(depositor);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.emergencyWithdrawAll();
        vm.stopPrank();

        (bool legacy, ) = address(bridge).call(
            abi.encodeWithSelector(LEGACY_WITHDRAW_SELECTOR, 0, depositor, AMOUNT, 1, hex"")
        );
        assertFalse(legacy);
        assertEq(usdc.balanceOf(depositor), 0);
    }
}
