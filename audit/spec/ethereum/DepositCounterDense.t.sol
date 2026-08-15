// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/ReturnlessERC20.sol";

/// @title DepositCounterDenseTest
/// @notice TD-62 — L1 `depositId` dense `0..n-1`; failed deposit does not increment counter.
contract DepositCounterDenseTest is Test {
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

    function test_td62_n_deposits_ids_are_zero_through_n_minus_one() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0xA11CE);
        bytes32 anAccount = bytes32(uint256(uint160(user)));

        uint256 n = 5;
        uint256 total = (n * (n + 1) / 2) * UsdcTestLib.UNIT;

        usdc.mint(user, total);
        vm.startPrank(user);
        usdc.approve(address(bridge), total);
        for (uint256 i = 0; i < n; i++) {
            uint256 amount = (i + 1) * UsdcTestLib.UNIT;
            vm.expectEmit(true, true, false, true);
            emit Deposit(i, user, amount, int8(0), anAccount, block.timestamp);
            bridge.deposit(amount, int8(0), anAccount);
            assertEq(bridge.depositCounter(), i + 1, "TD-62 counter after deposit");
        }
        vm.stopPrank();

        assertEq(bridge.depositCounter(), n, "TD-62 dense counter == n");
    }

    function test_td62_no_emit_without_counter_increment() public {
        ReturnlessERC20 token = new ReturnlessERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xD00D);

        token.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 0, "TD-62 failed deposit: counter");
        assertEq(bridge.treasuryBalance(), 0, "TD-62 failed deposit: treasury");
    }
}
