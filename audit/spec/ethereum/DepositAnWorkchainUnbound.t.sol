// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositAnWorkchainUnboundTest
/// @notice TD-31 — `anWorkchain` emitted on L1, not in ZK PI; AN credits WC 0 (QC-AN-J4).
contract DepositAnWorkchainUnboundTest is Test {
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
        usdc.mint(user, 10_000 * 1e6);
    }

    /// Fuzz full `int8` range: L1 never rejects; treasury always += amount.
    function testFuzz_td31_deposit_anyWorkchain_treasuryIncreases(int8 workchain, uint256 amount) public {
        amount = bound(amount, 1, 1_000 * 1e6);
        bytes32 anAccount = bytes32(uint256(0xABCD));

        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, amount, workchain, anAccount, block.timestamp);
        bridge.deposit(amount, workchain, anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), treasuryBefore + amount);
    }

    function test_td31_l1DoesNotStoreWorkchainInBridgeState() public {
        bytes32 acct = bytes32(uint256(0x42));
        vm.startPrank(user);
        usdc.approve(address(bridge), 2e6);
        bridge.deposit(1e6, int8(127), acct);
        bridge.deposit(1e6, int8(-1), acct);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 2);
        assertEq(bridge.treasuryBalance(), 2e6);
        // No bridge storage keyed by workchain — only counter + treasury move.
    }

    function test_td31_extremeWorkchains_emitAndAccept() public {
        bytes32 acct = bytes32(uint256(0x99));
        int8[3] memory cases = [int8(127), int8(-128), int8(-1)];

        vm.startPrank(user);
        usdc.approve(address(bridge), 3e6);
        for (uint256 i = 0; i < cases.length; i++) {
            bridge.deposit(1e6, cases[i], acct);
        }
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 3e6);
    }
}
