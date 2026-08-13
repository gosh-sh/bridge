// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositCounterTest
/// @notice Phase C / A1 — DEP-5: monotonic depositCounter and event depositId binding.
contract DepositCounterTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

    address internal userA = address(0xA11CE);
    address internal userB = address(0xB0B);

    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

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

    function test_depositCounter_monotonic() public {
        assertEq(bridge.depositCounter(), 0, "genesis counter");

        usdc.mint(userA, 200 * UsdcTestLib.UNIT);
        vm.startPrank(userA);
        usdc.approve(address(bridge), 200 * UsdcTestLib.UNIT);

        vm.expectEmit(true, true, false, true);
        emit Deposit(0, userA, UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(userA))), block.timestamp);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(userA))));

        vm.expectEmit(true, true, false, true);
        emit Deposit(1, userA, 2 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(userB))), block.timestamp);
        bridge.deposit(2 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(userB))));
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 2, "counter advanced twice");
        assertEq(bridge.treasuryBalance(), 3 * UsdcTestLib.UNIT, "ledger matches sum");
    }
}
