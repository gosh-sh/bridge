// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/ReturnlessERC20.sol";
import "@bridge-test/mocks/ReentrantERC20.sol";

/// @title DepositMutationKillTest
/// @notice TD-49 — L1 `deposit()` mutants killed by invariant / overlay / reentrancy suite.
/// @dev Mutant → test mapping in `audit/reports/td-49-mutation-score-notes.md`.
contract DepositMutationKillTest is Test {
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

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

    /// @dev Mutant: skip `transferFrom` — killed by counter/treasury freeze (TR-1 overlay).
    function test_td49_mutant_no_transfer_treasury_and_counter_unchanged() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0x1111);

        usdc.mint(user, 10 * UsdcTestLib.UNIT);
        vm.startPrank(user);
        usdc.approve(address(bridge), 5 * UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(10 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 0, "TD-49 kill: counter");
        assertEq(bridge.treasuryBalance(), 0, "TD-49 kill: treasury");
        assertEq(usdc.balanceOf(address(bridge)), 0, "TD-49 kill: custody");
    }

    /// @dev Mutant: accept deposit without token movement (returnless) — revert, no ledger credit.
    function test_td49_mutant_returnless_transfer_kill() public {
        ReturnlessERC20 token = new ReturnlessERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xD00D);

        token.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 0, "TD-49 kill: returnless");
        assertEq(bridge.treasuryBalance(), 0, "TD-49 kill: TR-1 ledger");
    }

    /// @dev Mutant: credit treasury without `Deposit` emit — overlay `expectEmit` + DEP-5 counter.
    function test_td49_mutant_missing_emit_breaks_overlay() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0xA11CE);
        bytes32 anAccount = bytes32(uint256(uint160(user)));

        usdc.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);

        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, UsdcTestLib.UNIT, int8(0), anAccount, block.timestamp);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 1, "TD-49 overlay: counter");
        assertEq(bridge.treasuryBalance(), UsdcTestLib.UNIT, "TD-49 overlay: treasury");
    }

    /// @dev Control — successful deposit grows treasury with custody (TR-1 positive).
    function test_td49_happy_path_treasury_matches_transfer() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0xBEEF);

        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 3 * UsdcTestLib.UNIT);

        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), 3 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 3 * UsdcTestLib.UNIT, "TD-49 TR-1 custody");
    }

    /// @dev Mutant: remove reentrancy guard — killed by TD-25 `DepositCrossFnReentrancy.t.sol` (reference).
    function test_td49_mutant_reentrancy_guard_reference_td25() public {
        ReentrantERC20 token = new ReentrantERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xBEEF);
        bytes32 anAccount = bytes32(uint256(0x1234));

        token.mint(user, UsdcTestLib.UNIT);
        token.wireReenter(bridge, anAccount);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.Reentrancy.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 0, "TD-49 ref TD-25");
    }
}
