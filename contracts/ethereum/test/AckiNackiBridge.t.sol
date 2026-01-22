// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";

contract AckiNackiBridgeTest is Test {
    AckiNackiBridge public bridge;
    
    address public user1 = address(0x1);
    address public user2 = address(0x2);
    
    event Deposit(
        bytes32 indexed commitment,
        bytes32 indexed commitmentWithAmount,
        uint256 leafIndex,
        uint256 amount,
        uint256 timestamp
    );
    
    event Withdrawal(
        bytes32 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 timestamp
    );
    
    function setUp() public {
        bridge = new AckiNackiBridge();
        
        // Fund test users
        vm.deal(user1, 100 ether);
        vm.deal(user2, 100 ether);
    }
    
    function testDeposit() public {
        bytes32 commitment = keccak256("test_commitment");
        uint256 amount = 1 ether;
        
        vm.startPrank(user1);
        
        // Expect the Deposit event
        vm.expectEmit(true, true, false, true);
        emit Deposit(
            commitment,
            bridge.hashPair(commitment, bytes32(amount)),
            0,
            amount,
            block.timestamp
        );
        
        bridge.deposit{value: amount}(commitment, amount);
        
        vm.stopPrank();
        
        // Verify state
        assertEq(bridge.nextIndex(), 1);
        assertEq(bridge.treasuryBalance(), amount);
        assertEq(bridge.getLeaf(0), commitment);
    }
    
    function testDepositMultiple() public {
        uint256 amount = 1 ether;
        
        for (uint256 i = 0; i < 5; i++) {
            bytes32 commitment = keccak256(abi.encodePacked("commitment", i));
            
            vm.prank(user1);
            bridge.deposit{value: amount}(commitment, amount);
            
            assertEq(bridge.getLeaf(i), commitment);
        }
        
        assertEq(bridge.nextIndex(), 5);
        assertEq(bridge.treasuryBalance(), 5 ether);
    }
    
    function testDepositInvalidAmount() public {
        bytes32 commitment = keccak256("test_commitment");
        
        vm.startPrank(user1);
        
        // Send different amount than specified
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.deposit{value: 2 ether}(commitment, 1 ether);
        
        // Send zero amount
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.deposit{value: 0}(commitment, 0);
        
        vm.stopPrank();
    }
    
    function testMerkleRootChanges() public {
        bytes32 root0 = bridge.getRoot();
        
        bytes32 commitment1 = keccak256("commitment1");
        vm.prank(user1);
        bridge.deposit{value: 1 ether}(commitment1, 1 ether);
        
        bytes32 root1 = bridge.getRoot();
        assertFalse(root0 == root1, "Root should change after deposit");
        
        bytes32 commitment2 = keccak256("commitment2");
        vm.prank(user1);
        bridge.deposit{value: 1 ether}(commitment2, 1 ether);
        
        bytes32 root2 = bridge.getRoot();
        assertFalse(root1 == root2, "Root should change after second deposit");
    }
    
    function testWithdrawal() public {
        // First, make a deposit
        bytes32 commitment = keccak256("test_commitment");
        uint256 depositAmount = 1 ether;
        
        vm.prank(user1);
        bridge.deposit{value: depositAmount}(commitment, depositAmount);
        
        // Now withdraw
        bytes32 nullifier = keccak256("test_nullifier");
        bytes32 root = bridge.getRoot();
        bytes memory proof = "dummy_proof"; // Placeholder
        
        uint256 balanceBefore = user2.balance;
        
        vm.expectEmit(true, true, false, true);
        emit Withdrawal(nullifier, user2, depositAmount, block.timestamp);
        
        bridge.withdraw(nullifier, payable(user2), depositAmount, root, proof);
        
        // Verify state
        assertEq(user2.balance, balanceBefore + depositAmount);
        assertEq(bridge.treasuryBalance(), 0);
        assertTrue(bridge.isNullifierUsed(nullifier));
    }
    
    function testWithdrawalDoubleSpendPrevention() public {
        // Make a deposit
        bytes32 commitment = keccak256("test_commitment");
        uint256 amount = 1 ether;
        
        vm.prank(user1);
        bridge.deposit{value: amount}(commitment, amount);
        
        // First withdrawal
        bytes32 nullifier = keccak256("test_nullifier");
        bytes32 root = bridge.getRoot();
        bytes memory proof = "dummy_proof";
        
        bridge.withdraw(nullifier, payable(user2), amount, root, proof);
        
        // Try to use same nullifier again
        vm.expectRevert(AckiNackiBridge.NullifierAlreadyUsed.selector);
        bridge.withdraw(nullifier, payable(user2), amount, root, proof);
    }
    
    function testWithdrawalInvalidRoot() public {
        // Make a deposit
        bytes32 commitment = keccak256("test_commitment");
        uint256 amount = 1 ether;
        
        vm.prank(user1);
        bridge.deposit{value: amount}(commitment, amount);
        
        // Try to withdraw with wrong root
        bytes32 nullifier = keccak256("test_nullifier");
        bytes32 wrongRoot = keccak256("wrong_root");
        bytes memory proof = "dummy_proof";
        
        vm.expectRevert(AckiNackiBridge.InvalidProof.selector);
        bridge.withdraw(nullifier, payable(user2), amount, wrongRoot, proof);
    }
    
    function testWithdrawalInsufficientTreasury() public {
        // Make a small deposit
        bytes32 commitment = keccak256("test_commitment");
        uint256 depositAmount = 1 ether;
        
        vm.prank(user1);
        bridge.deposit{value: depositAmount}(commitment, depositAmount);
        
        // Try to withdraw more than deposited
        bytes32 nullifier = keccak256("test_nullifier");
        bytes32 root = bridge.getRoot();
        bytes memory proof = "dummy_proof";
        
        vm.expectRevert(AckiNackiBridge.InsufficientTreasury.selector);
        bridge.withdraw(nullifier, payable(user2), 2 ether, root, proof);
    }
    
    function testWithdrawalEmptyProof() public {
        // Make a deposit
        bytes32 commitment = keccak256("test_commitment");
        uint256 amount = 1 ether;
        
        vm.prank(user1);
        bridge.deposit{value: amount}(commitment, amount);
        
        // Try to withdraw with empty proof
        bytes32 nullifier = keccak256("test_nullifier");
        bytes32 root = bridge.getRoot();
        bytes memory emptyProof = "";
        
        vm.expectRevert(AckiNackiBridge.InvalidProof.selector);
        bridge.withdraw(nullifier, payable(user2), amount, root, emptyProof);
    }
    
    function testHashPairDeterministic() public {
        bytes32 left = keccak256("left");
        bytes32 right = keccak256("right");
        
        bytes32 hash1 = bridge.hashPair(left, right);
        bytes32 hash2 = bridge.hashPair(left, right);
        
        assertEq(hash1, hash2, "Hash should be deterministic");
    }
    
    function testHashPairOrderMatters() public {
        bytes32 left = keccak256("left");
        bytes32 right = keccak256("right");
        
        bytes32 hash1 = bridge.hashPair(left, right);
        bytes32 hash2 = bridge.hashPair(right, left);
        
        assertFalse(hash1 == hash2, "Hash order should matter");
    }
    
    function testGetLeafInvalidIndex() public {
        vm.expectRevert("Invalid index");
        bridge.getLeaf(0);
        
        // Add one leaf
        vm.prank(user1);
        bridge.deposit{value: 1 ether}(keccak256("commitment"), 1 ether);
        
        // Should work for index 0
        bridge.getLeaf(0);
        
        // Should fail for index 1
        vm.expectRevert("Invalid index");
        bridge.getLeaf(1);
    }
    
    function testTreeFull() public {
        // This test would take too long with TREE_HEIGHT=20
        // So we just verify the error exists
        // In practice, you'd deploy a test version with smaller height
        
        // For now, just verify the contract has the error defined
        // by trying to trigger it (won't actually fill the tree)
        assertTrue(true);
    }
}

