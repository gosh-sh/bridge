// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/console.sol";
import "../src/AckiNackiBridge.sol";
import "../src/IAckiNackiVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";

/**
 * @title AckiNackiBridgeV2Test
 * @notice Tests for the simplified event-based bridge (no Merkle tree)
 * @dev Tests the new design where deposits emit events and withdrawals verify ZK proofs
 */
contract AckiNackiBridgeV2Test is Test {
    AckiNackiBridge public bridge;
    TestDepositVerifier public verifier;

    address public user1 = address(0x1234);
    address public user2 = address(0x5678);

    // Events (must match contract)
    event Deposit(
        uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp
    );

    event Withdrawal(
        uint256 indexed depositId, address indexed recipient, uint256 amount, uint256 timestamp
    );

    function setUp() public {
        // Deploy test verifier (accepts any valid proof format)
        verifier = new TestDepositVerifier();

        // Deploy mock block header oracle
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();

        // Deploy bridge with verifier and oracle addresses (AAVE disabled)
        bridge = new AckiNackiBridge(
            address(verifier), address(oracle), address(0), address(0), address(0)
        );

        // Fund test users
        vm.deal(user1, 100 ether);
        vm.deal(user2, 100 ether);
    }

    // ============ Deposit Tests ============

    function testDeposit() public {
        uint256 amount = 1 ether;

        vm.startPrank(user1);

        // Expect the Deposit event
        vm.expectEmit(true, true, false, true);
        emit Deposit(
            0, // depositId (first deposit)
            user1, // sender
            amount, // amount
            block.timestamp // timestamp
        );

        bridge.deposit{ value: amount }();

        vm.stopPrank();

        // Verify state
        assertEq(bridge.depositCounter(), 1, "Deposit counter should be 1");
        assertEq(bridge.treasuryBalance(), amount, "Treasury should have deposit amount");
        assertEq(address(bridge).balance, amount, "Bridge should hold the ETH");
    }

    function testDepositMultiple() public {
        uint256 amount = 1 ether;

        for (uint256 i = 0; i < 5; i++) {
            vm.prank(user1);

            vm.expectEmit(true, true, false, true);
            emit Deposit(i, user1, amount, block.timestamp);

            bridge.deposit{ value: amount }();
        }

        assertEq(bridge.depositCounter(), 5, "Should have 5 deposits");
        assertEq(bridge.treasuryBalance(), 5 ether, "Treasury should have 5 ETH");
    }

    function testDepositInvalidAmount() public {
        vm.startPrank(user1);

        // Send zero amount
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.deposit{ value: 0 }();

        vm.stopPrank();
    }

    function testDepositTooLarge() public {
        vm.startPrank(user1);
        vm.deal(user1, 200 ether);

        // Send more than MAX_DEPOSIT_AMOUNT (100 ether)
        vm.expectRevert(AckiNackiBridge.DepositTooLarge.selector);
        bridge.deposit{ value: 101 ether }();

        vm.stopPrank();
    }

    function testDepositCounterIncrement() public {
        assertEq(bridge.depositCounter(), 0, "Initial counter should be 0");

        vm.prank(user1);
        bridge.deposit{ value: 1 ether }();
        assertEq(bridge.depositCounter(), 1, "Counter should be 1 after first deposit");

        vm.prank(user2);
        bridge.deposit{ value: 2 ether }();
        assertEq(bridge.depositCounter(), 2, "Counter should be 2 after second deposit");
    }

    function testDepositFromDifferentUsers() public {
        uint256 amount1 = 1 ether;
        uint256 amount2 = 2 ether;

        // User1 deposits
        vm.prank(user1);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user1, amount1, block.timestamp);
        bridge.deposit{ value: amount1 }();

        // User2 deposits
        vm.prank(user2);
        vm.expectEmit(true, true, false, true);
        emit Deposit(1, user2, amount2, block.timestamp);
        bridge.deposit{ value: amount2 }();

        assertEq(bridge.depositCounter(), 2);
        assertEq(bridge.treasuryBalance(), amount1 + amount2);
    }

    // ============ Withdrawal Tests ============

    function testWithdrawal() public {
        uint256 depositAmount = 1 ether;
        uint256 depositId = 0;

        // First, make a deposit
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        // Create a valid proof (test verifier accepts any non-empty proof)
        bytes memory proof = hex"0123456789abcdef"; // Dummy proof

        // Prepare public inputs: [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = depositAmount;
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128); // High 128 bits
        publicInputs[5] = uint256(uint128(uint256(blockHash))); // Low 128 bits

        // Record user1 balance before withdrawal
        uint256 balanceBefore = user1.balance;

        // Withdraw
        vm.expectEmit(true, true, false, true);
        emit Withdrawal(depositId, user1, depositAmount, block.timestamp);

        bridge.withdraw(payable(user1), depositAmount, depositId, blockNumber, proof);

        // Verify state
        assertEq(user1.balance, balanceBefore + depositAmount, "User should receive deposit amount");
        assertEq(bridge.treasuryBalance(), 0, "Treasury should be empty");
        assertTrue(bridge.processedDeposits(depositId), "Deposit should be marked as processed");
    }

    function testWithdrawalDoubleSpend() public {
        uint256 depositAmount = 1 ether;
        uint256 depositId = 0;

        // Make a deposit
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        // Create proof
        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = depositAmount;
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        // First withdrawal succeeds
        bridge.withdraw(payable(user1), depositAmount, depositId, blockNumber, proof);

        // Second withdrawal with same depositId should fail
        vm.expectRevert(AckiNackiBridge.DepositAlreadyProcessed.selector);
        bridge.withdraw(payable(user1), depositAmount, depositId, blockNumber, proof);
    }

    function testWithdrawalInvalidProof() public {
        uint256 depositAmount = 1 ether;
        uint256 depositId = 0;

        // Make a deposit
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        // Create INVALID proof (empty)
        bytes memory proof = hex"";
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = depositAmount;
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        // Withdrawal should fail
        vm.expectRevert(AckiNackiBridge.InvalidProof.selector);
        bridge.withdraw(payable(user1), depositAmount, depositId, blockNumber, proof);
    }

    function testWithdrawalInsufficientTreasury() public {
        uint256 depositAmount = 1 ether;
        uint256 withdrawAmount = 2 ether; // More than deposited
        uint256 depositId = 0;

        // Make a deposit
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        // Try to withdraw more than treasury has
        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = withdrawAmount; // More than deposited!
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        vm.expectRevert(AckiNackiBridge.InsufficientTreasury.selector);
        bridge.withdraw(payable(user1), withdrawAmount, depositId, blockNumber, proof);
    }

    function testWithdrawalInvalidRecipient() public {
        uint256 depositAmount = 1 ether;
        uint256 depositId = 0;

        // Make a deposit
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        // Try to withdraw to zero address
        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = depositAmount;
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        vm.expectRevert(AckiNackiBridge.InvalidRecipient.selector);
        bridge.withdraw(payable(address(0)), depositAmount, depositId, blockNumber, proof);
    }

    function testIsDepositProcessed() public {
        uint256 depositAmount = 1 ether;
        uint256 depositId = 0;

        // Initially not processed
        assertFalse(bridge.isDepositProcessed(depositId));

        // Make deposit and withdraw
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();

        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;
        bytes32 blockHash = blockhash(blockNumber);

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(user1));
        publicInputs[2] = depositAmount;
        publicInputs[3] = uint256(uint160(address(bridge)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        bridge.withdraw(payable(user1), depositAmount, depositId, blockNumber, proof);

        // Now should be processed
        assertTrue(bridge.isDepositProcessed(depositId));
    }

    // ============ Constructor Tests ============

    function testConstructorInvalidVerifier() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        vm.expectRevert(AckiNackiBridge.InvalidVerifier.selector);
        new AckiNackiBridge(address(0), address(oracle), address(0), address(0), address(0));
    }

    function testConstructorInvalidOracle() public {
        vm.expectRevert(AckiNackiBridge.InvalidOracle.selector);
        new AckiNackiBridge(address(verifier), address(0), address(0), address(0), address(0));
    }
}

/**
 * @title TestDepositVerifier
 * @notice Simple test verifier for deposit proofs (TESTING ONLY)
 * @dev Accepts any proof with valid format for testing purposes
 */
contract TestDepositVerifier is IAckiNackiVerifier {
    uint256 private constant PUBLIC_INPUTS_COUNT = 6; // depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow

    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        pure
        override
        returns (bool isValid, bytes32 depositId)
    {
        // Validate proof is not empty
        if (proof.length == 0) {
            return (false, bytes32(0));
        }

        // Validate public inputs count
        if (publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }

        // Extract public inputs
        uint256 depositIdValue = publicInputs[0];
        uint256 sender = publicInputs[1];
        uint256 amount = publicInputs[2];
        uint256 contractAddress = publicInputs[3];

        // Basic validation
        if (sender == 0 || amount == 0 || contractAddress == 0) {
            return (false, bytes32(0));
        }

        // TEST MODE: Accept any proof with valid format
        return (true, bytes32(depositIdValue));
    }

    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}

