// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositCrossCapPolicyTest
/// @notice TD-36 — L1 per-tx `uint64.max` vs uncapped aggregate (QC-A1-1 / QC-AN-J1).
/// @dev Cross-ref: `DepositWhaleCap.t.sol`, TD-33 dust aggregate.
contract DepositCrossCapPolicyTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

    uint256 internal maxPerTx;

    function setUp() public {
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc = new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        maxPerTx = bridge.MAX_DEPOSIT_AMOUNT();
        assertEq(maxPerTx, type(uint64).max, "TD-36: L1 per-tx cap is uint64.max");
    }

    function test_td36_per_tx_max_amount_succeeds() public {
        address user = address(0xCAFE);
        usdc.mint(user, maxPerTx);
        vm.startPrank(user);
        usdc.approve(address(bridge), maxPerTx);
        bridge.deposit(maxPerTx, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), maxPerTx);
        assertEq(usdc.balanceOf(address(bridge)), maxPerTx);
    }

    function test_td36_over_per_tx_max_reverts() public {
        address user = address(0xBEEF);
        vm.startPrank(user);
        usdc.approve(address(bridge), maxPerTx + 1);
        vm.expectRevert(AckiNackiBridge.DepositTooLarge.selector);
        bridge.deposit(maxPerTx + 1, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_td36_n_deposits_aggregate_treasury_uncapped_on_l1() public {
        uint256 chunk = 1000 * UsdcTestLib.UNIT;
        uint256 n = 15;
        uint256 expected = n * chunk;

        for (uint256 i = 0; i < n; i++) {
            address user = address(uint160(0x3000 + i));
            UsdcTestLib.depositUsdc(vm, usdc, bridge, user, chunk);
        }

        assertEq(bridge.depositCounter(), n);
        assertEq(bridge.treasuryBalance(), expected, "QC-A1-1: L1 aggregate uncapped");
        assertEq(usdc.balanceOf(address(bridge)), expected);
        assertGt(maxPerTx, expected, "aggregate can grow below per-tx bound");
    }

    function test_td36_many_max_per_tx_deposits_aggregate_uncapped() public {
        uint256 n = 5;
        uint256 expected = n * maxPerTx;

        for (uint256 i = 0; i < n; i++) {
            address user = address(uint160(0x4000 + i));
            usdc.mint(user, maxPerTx);
            vm.startPrank(user);
            usdc.approve(address(bridge), maxPerTx);
            bridge.deposit(maxPerTx, int8(0), bytes32(uint256(uint160(user))));
            vm.stopPrank();
        }

        assertEq(bridge.treasuryBalance(), expected, "TD-36: repeated max-per-tx still uncapped");
        assertEq(bridge.depositCounter(), n);
    }
}
