// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositWorkchainTest
/// @notice Phase G — `anWorkchain` is emitted but not validated on L1 (QC-AN-J4 closed on AN).
/// @dev INV: DEP-1
contract DepositWorkchainTest is Test {
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

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
        usdc.mint(user, 1_000 * 1e6);
    }

    function testFuzz_deposit_anyWorkchain_emitted(int8 workchain) public {
        uint256 amount = 1e6;
        bytes32 anAccount = bytes32(uint256(0xABCD));

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, amount, workchain, anAccount, block.timestamp);
        bridge.deposit(amount, workchain, anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), amount);
    }

    function test_deposit_distinctAnAccounts_incrementCounter() public {
        bytes32 acctA = bytes32(uint256(0xA));
        bytes32 acctB = bytes32(uint256(0xB));

        vm.startPrank(user);
        usdc.approve(address(bridge), 4e6);
        bridge.deposit(1e6, int8(0), acctA);
        bridge.deposit(2e6, int8(-1), acctB);
        bridge.deposit(1e6, int8(127), acctA);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 3);
        assertEq(bridge.treasuryBalance(), 4e6);
    }
}
