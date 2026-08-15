// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositEip1559OnlyTest
/// @notice TD-12 — L1 `deposit()` succeeds regardless of caller tx envelope; prover binds EIP-1559 `0x02` only.
/// @dev QC gap: `deposit-prover/tests/td_12_eip1559_only.rs` proves non-0x02 witnesses fail-closed.
contract DepositEip1559OnlyTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    address internal user = address(0xBEEF);

    function setUp() public {
        usdc = new MockERC20("USDC", "USDC", 6);
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

  function test_td12_deposit_succeeds_on_l1_default_tx() public {
        bytes32 anAccount = bytes32(uint256(0x1559));
        uint256 amount = UsdcTestLib.UNIT;

        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), amount);
        assertEq(usdc.balanceOf(address(bridge)), amount);
    }
}
