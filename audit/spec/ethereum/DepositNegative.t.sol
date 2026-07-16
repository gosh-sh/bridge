// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositNegativeTest
/// @notice Phase C / A1 — negative deposit paths flagged in manual audit.
/// @dev INV: DEP-1, DEP-2
contract DepositNegativeTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;

    address internal user = address(0xBEEF);

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        usdc.mint(user, 200 * UsdcTestLib.UNIT);
    }

    /// @dev INV: DEP-1 — gap in existing suite (A1 manual audit)
    function test_deposit_zeroAnAccount_reverts() public {
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.InvalidAnAccount.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(0));
        vm.stopPrank();
    }

    /// @dev INV: DEP-2
    function test_deposit_zeroAmount_reverts() public {
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.deposit(0, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    /// @dev INV: DEP-2 — MAX_DEPOSIT_AMOUNT = 100 USDC
    function test_deposit_exceedsMax_reverts() public {
        uint256 over = bridge.MAX_DEPOSIT_AMOUNT() + 1;
        usdc.mint(user, over);
        vm.startPrank(user);
        usdc.approve(address(bridge), over);
        vm.expectRevert(AckiNackiBridge.DepositTooLarge.selector);
        bridge.deposit(over, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }
}
