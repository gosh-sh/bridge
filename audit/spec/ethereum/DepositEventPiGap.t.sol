// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositEventPiGapTest
/// @notice TD-44 — event `anWorkchain` (data word 1) and `timestamp` (data word 3) are not ZK PI slots.
///
/// Event field → PI slot (12 Fr layout):
/// | Event field        | PI slot | Binding |
/// |--------------------|---------|---------|
/// | depositId (topic1) | 0       | bound   |
/// | sender (topic2)    | 1       | bound   |
/// | amount (data w0)   | 2       | bound   |
/// | contract (log addr)| 3       | bound   |
/// | chainId (tx RLP)   | 4       | bound   |
/// | anWorkchain (w1)   | none    | not PI (TD-31) |
/// | anAccount (data w2)| 7–8     | bound   |
/// | timestamp (data w3)| none    | not PI (TD-44) |
/// | blockHash          | 9–10    | indirect via header RLP keccak (TD-32 slot 11) |
/// | dappId             | 5–6     | config witness, not in event |
/// | promiseCommit      | 11      | circuit-only, not in event (TD-21) |
contract DepositEventPiGapTest is Test {
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

    /// Different `block.timestamp` / warp: treasury and counter move; event timestamp is emitted only.
    function test_td44_event_timestamp_not_in_pi_slots() public {
        bytes32 acct = bytes32(uint256(0xABCD));
        uint256 amount = 3e6;

        vm.warp(1_000_000_000);
        uint256 t0 = block.timestamp;
        assertEq(t0, 1_000_000_000);

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);

        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, 1e6, 0, acct, t0);
        bridge.deposit(1e6, 0, acct);

        vm.warp(2_000_000_000);
        uint256 t1 = block.timestamp;
        assertEq(t1, 2_000_000_000);

        vm.expectEmit(true, true, false, true);
        emit Deposit(1, user, 2e6, 0, acct, t1);
        bridge.deposit(2e6, 0, acct);
        vm.stopPrank();

        assertEq(bridge.depositCounter(), 2);
        assertEq(bridge.treasuryBalance(), amount);
    }

    /// Fuzz WC + warp: L1 always accepts (cross-ref TD-31).
    function testFuzz_td44_anWorkchain_and_timestamp_fuzz_independent(
        int8 workchain,
        uint256 warpSeconds,
        uint256 amount
    ) public {
        warpSeconds = bound(warpSeconds, 1, 365 * 24 * 60 * 60);
        amount = bound(amount, 1, 1_000 * 1e6);
        bytes32 acct = bytes32(uint256(0x55AA));

        vm.warp(block.timestamp + warpSeconds);
        uint256 emitTs = block.timestamp;

        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, amount, workchain, acct, emitTs);
        bridge.deposit(amount, workchain, acct);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), treasuryBefore + amount);
    }
}
