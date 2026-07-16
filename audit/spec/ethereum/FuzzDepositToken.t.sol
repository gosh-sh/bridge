// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title FuzzDepositTokenTest
/// @notice Phase D — F-TR-4 / DEP-3: exact transferFrom amount matches treasury credit.
/// @dev BOUNDS: amount ∈ [1, MAX_DEPOSIT_AMOUNT]
contract FuzzDepositTokenTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

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

    /// @dev INV: TR-4 / DEP-3
    function testFuzz_deposit_exactTransferFromAmount(uint256 rawAmount) public {
        uint256 amount = bound(rawAmount, 1, bridge.MAX_DEPOSIT_AMOUNT());
        address user = address(0xBEEF);

        usdc.mint(user, amount);
        uint256 bridgeBalBefore = usdc.balanceOf(address(bridge));
        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(usdc.balanceOf(address(bridge)), bridgeBalBefore + amount, "TR-4 token in");
        assertEq(bridge.treasuryBalance(), treasuryBefore + amount, "TR-4 ledger");
        assertEq(usdc.balanceOf(user), 0, "user debited exactly");
    }
}
