// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/ReturnlessERC20.sol";
import "@bridge-test/mocks/CrossFnReentrantERC20.sol";
import "@bridge-test/mocks/BlacklistableERC20.sol";

/// @title DepositStorageDiffTest
/// @notice TD-55 — failed `deposit()` leaves `depositCounter`, `treasuryBalance`, and
///         token custody unchanged (explicit storage-diff / ledger snapshot matrix).
contract DepositStorageDiffTest is Test {
    address internal user = address(0xA11CE);
    bytes32 internal anAccount = bytes32(uint256(uint160(user)));

    struct BridgeLedger {
        uint256 counter;
        uint256 treasury;
        uint256 custody;
    }

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

    function snapshotBridgeLedger(AckiNackiBridge bridge, address token)
        internal
        view
        returns (BridgeLedger memory ledger)
    {
        ledger.counter = bridge.depositCounter();
        ledger.treasury = bridge.treasuryBalance();
        ledger.custody = MockERC20(token).balanceOf(address(bridge));
    }

    function assertLedgerUnchanged(
        BridgeLedger memory before,
        BridgeLedger memory afterLedger,
        string memory label
    ) internal {
        assertEq(afterLedger.counter, before.counter, string.concat(label, ": counter"));
        assertEq(afterLedger.treasury, before.treasury, string.concat(label, ": treasury"));
        assertEq(afterLedger.custody, before.custody, string.concat(label, ": custody"));
    }

    function test_td55_fail_insufficient_allowance_ledger_frozen() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(usdc));

        vm.startPrank(user);
        usdc.approve(address(bridge), amount / 2);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(usdc)), "TD-55 allowance");
    }

    function test_td55_fail_insufficient_balance_ledger_frozen() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount / 2);
        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(usdc));

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(usdc)), "TD-55 balance");
    }

    function test_td55_fail_blacklist_ledger_frozen() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        token.mint(user, UsdcTestLib.UNIT);
        token.blacklist(user);

        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(token));

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Blacklisted.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(token)), "TD-55 blacklist");
    }

    function test_td55_fail_pause_ledger_frozen() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        token.mint(user, UsdcTestLib.UNIT);
        token.setPaused(true);

        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(token));

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Paused.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(token)), "TD-55 pause");
    }

    function test_td55_fail_returnless_token_ledger_frozen() public {
        ReturnlessERC20 token = new ReturnlessERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        token.mint(user, UsdcTestLib.UNIT);

        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(token));

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(token)), "TD-55 returnless");
    }

    function test_td55_fail_reentrant_deposit_ledger_frozen() public {
        CrossFnReentrantERC20 token = new CrossFnReentrantERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        token.wireBridge(bridge, address(0));
        token.wireDeposit(anAccount, UsdcTestLib.UNIT);
        token.mint(user, UsdcTestLib.UNIT);

        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(token));

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.Reentrancy.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(token)), "TD-55 reentrant");
    }

    function test_td55_success_deposit_moves_counter_treasury_custody() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = 3 * UsdcTestLib.UNIT;

        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(usdc));
        assertEq(before.counter, 0);
        assertEq(before.treasury, 0);
        assertEq(before.custody, 0);

        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, amount);

        BridgeLedger memory afterLedger = snapshotBridgeLedger(bridge, address(usdc));
        assertEq(afterLedger.counter, before.counter + 1, "TD-55 success: counter");
        assertEq(afterLedger.treasury, before.treasury + amount, "TD-55 success: treasury");
        assertEq(afterLedger.custody, before.custody + amount, "TD-55 success: custody");
    }

    function test_td55_fail_snapshot_revert_restores_ledger() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        BridgeLedger memory before = snapshotBridgeLedger(bridge, address(usdc));
        uint256 snap = vm.snapshotState();

        vm.startPrank(user);
        usdc.approve(address(bridge), 0);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        vm.revertToState(snap);
        assertLedgerUnchanged(before, snapshotBridgeLedger(bridge, address(usdc)), "TD-55 snapshot");
    }
}
