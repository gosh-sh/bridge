// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MultiLogERC20.sol";

/// @title DepositMultiLogCapacityTest
/// @notice TD-11 — L1 accepts deposit with 4+ receipt logs; prover caps at `MAX_LOG_NUM=3`.
/// @dev QC→BC gap documented: `deposit-prover/tests/td_11_capacity_bounds.rs` proves fail-closed.
contract DepositMultiLogCapacityTest is Test {
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    function test_td11_multiLogToken_deposit_succeeds_on_l1() public {
        MultiLogERC20 token = new MultiLogERC20();
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(token),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        address user = address(0xCAFE);
        bytes32 anAccount = bytes32(uint256(0xABCD));
        uint256 amount = UsdcTestLib.UNIT;

        token.mint(user, amount);
        vm.startPrank(user);
        token.approve(address(bridge), amount);

        vm.recordLogs();
        bridge.deposit(amount, int8(0), anAccount);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        vm.stopPrank();

        assertGe(logs.length, 4, "TD-11: receipt must carry 4+ logs (aux + Transfer + Deposit)");
        assertEq(bridge.treasuryBalance(), amount);
        assertEq(token.balanceOf(address(bridge)), amount);
    }
}
