// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/BlacklistableERC20.sol";

/// @title DepositUsdcBlacklistPauseTest
/// @notice TD-24 — USDC blacklist / pause mid-campaign; bridge has no deposit pause (#20).
/// @dev QC: token hooks fail-closed; ops must monitor USDC proxy pause/blacklist.
contract DepositUsdcBlacklistPauseTest is Test {
    address internal user = address(0xA11CE);
    bytes32 internal anAccount = bytes32(uint256(uint160(user)));

    function _bridge(BlacklistableERC20 token) internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(token),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    /// @dev (a) Blacklisted sender → `transferFrom` reverts → deposit fails closed.
    function test_td24_a_blacklistedUser_deposit_revertsOnTransferFrom() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(token);
        token.mint(user, UsdcTestLib.UNIT);
        token.blacklist(user);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Blacklisted.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);
        assertEq(token.balanceOf(address(bridge)), 0);
    }

    /// @dev (b) Global pause → deposit reverts on `transferFrom`.
    function test_td24_b_pausedToken_deposit_reverts() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(token);
        token.mint(user, UsdcTestLib.UNIT);
        token.setPaused(true);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Paused.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);
    }

    /// @dev (c) Post-deposit blacklist — no retroactive treasury/custody change.
    function test_td24_c_postDepositBlacklist_treasuryUnchanged() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(token);
        token.mint(user, UsdcTestLib.UNIT);

        UsdcTestLib.depositUsdc(vm, token, bridge, user, UsdcTestLib.UNIT);
        uint256 treasury = bridge.treasuryBalance();
        uint256 custody = token.balanceOf(address(bridge));

        token.blacklist(user);

        assertEq(bridge.treasuryBalance(), treasury, "blacklist does not claw back ledger");
        assertEq(token.balanceOf(address(bridge)), custody, "custody unchanged");
        assertEq(bridge.depositCounter(), 1);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Blacklisted.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();
    }

    /// @dev (d) Control — unblacklisted user deposits successfully.
    function test_td24_d_unblacklistedUser_deposit_ok() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(token);
        token.mint(user, UsdcTestLib.UNIT);

        UsdcTestLib.depositUsdc(vm, token, bridge, user, UsdcTestLib.UNIT);

        assertEq(bridge.treasuryBalance(), UsdcTestLib.UNIT);
        assertEq(token.balanceOf(address(bridge)), UsdcTestLib.UNIT);
        assertEq(bridge.depositCounter(), 1);
    }

    /// @dev Blacklist bridge recipient blocks inbound `transferFrom` (paused path variant).
    function test_td24_blacklistedBridgeRecipient_deposit_reverts() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(token);
        token.mint(user, UsdcTestLib.UNIT);
        token.blacklist(address(bridge));

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Blacklisted.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();
    }
}
