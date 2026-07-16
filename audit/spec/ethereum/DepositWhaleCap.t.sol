// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositWhaleCapTest
/// @notice Phase C / A1 — QC-A1-1: per-tx cap 100 USDC; aggregate deposit volume unbounded.
/// @dev Documents current policy — no daily/global cap on treasury inflow.
contract DepositWhaleCapTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

    uint256 internal constant MAX = 100 * UsdcTestLib.UNIT;

    function setUp() public {
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc = new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_manyDeposits_aggregateUnbounded() public {
        uint256 n = 20;
        uint256 expectedTotal = n * MAX;

        for (uint256 i = 0; i < n; i++) {
            address user = address(uint160(0x1000 + i));
            UsdcTestLib.depositUsdc(vm, usdc, bridge, user, MAX);
        }

        assertEq(bridge.depositCounter(), n, "counter");
        assertEq(bridge.treasuryBalance(), expectedTotal, "QC-A1-1: aggregate uncapped");
        assertEq(usdc.balanceOf(address(bridge)), expectedTotal, "custody matches ledger");
    }
}
