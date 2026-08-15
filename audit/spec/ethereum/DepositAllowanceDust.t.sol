// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositAllowanceDustTest
/// @notice TD-61 — INV: exact `approve` / allowance dust paths for `deposit()` +
///         `transferFrom(sender, bridge, amount)` (TR-1).
contract DepositAllowanceDustTest is Test {
    address internal user = address(0xA11CE);
    bytes32 internal anAccount = bytes32(uint256(uint160(user)));

    function _bridge(address token) internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            token,
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

  /// (a) approve == amount → deposit Ok; allowance 0 after (standard ERC20).
    function test_td61_approve_equals_amount_ok_allowance_zero() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(usdc.allowance(user, address(bridge)), 0, "TD-61(a): allowance exhausted");
        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), amount);
        assertEq(usdc.balanceOf(address(bridge)), amount);
    }

  /// (b) approve > amount → Ok; allowance reduced by amount; treasury/counter += amount.
    function test_td61_approve_exceeds_amount_reduces_allowance() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;
        uint256 surplus = 5 * UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount + surplus);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(
            usdc.allowance(user, address(bridge)),
            surplus,
            "TD-61(b): allowance reduced by deposit amount"
        );
        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), amount);
        assertEq(usdc.balanceOf(address(bridge)), amount);
    }

  /// (c) Second deposit same size without re-approve → revert if allowance exhausted.
    function test_td61_second_deposit_without_reapprove_reverts() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, 2 * amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(usdc.allowance(user, address(bridge)), 0);
        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), amount);
    }

  /// (d) approve(type(uint256).max) → two deposits same amount both Ok; allowance still large.
    function test_td61_max_allowance_two_deposits_ok() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, 2 * amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), type(uint256).max);
        bridge.deposit(amount, int8(0), anAccount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(usdc.allowance(user, address(bridge)), type(uint256).max);
        assertEq(bridge.depositCounter(), 2);
        assertEq(bridge.treasuryBalance(), 2 * amount);
        assertEq(usdc.balanceOf(address(bridge)), 2 * amount);
    }

  /// (e) approve(amount-1) → revert; counter/treasury unchanged (TD-55 overlap OK).
    function test_td61_approve_amount_minus_one_reverts_ledger_frozen() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount - 1);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 0);
        assertEq(bridge.treasuryBalance(), 0);
        assertEq(usdc.balanceOf(address(bridge)), 0);
        assertEq(usdc.allowance(user, address(bridge)), amount - 1);
    }

  /// (f) Dust: approve amount+1, deposit amount → allowance 1 left; micro-deposit 1 wei Ok.
    function test_td61_dust_remainder_micro_deposit_succeeds() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount + 1);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount + 1);
        bridge.deposit(amount, int8(0), anAccount);
        assertEq(usdc.allowance(user, address(bridge)), 1, "TD-61(f): 1 wei dust allowance");

        bridge.deposit(1, int8(0), anAccount);
        vm.stopPrank();

        assertEq(usdc.allowance(user, address(bridge)), 0, "TD-61(f): dust consumed");
        assertEq(bridge.depositCounter(), 2);
        assertEq(bridge.treasuryBalance(), amount + 1);
        assertEq(usdc.balanceOf(address(bridge)), amount + 1);
    }
}
