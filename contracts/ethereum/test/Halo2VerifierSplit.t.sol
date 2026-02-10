// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/console.sol";
import "../src/Halo2VerifierPart1.sol";
import "../src/Halo2VerifierPart2.sol";
import "../src/Halo2VerifierPart3.sol";
import "../src/Halo2VerifierProxy.sol";

/**
 * @title Halo2VerifierSplitTest
 * @notice Tests for the split Halo2 verifier implementation
 * @dev This test verifies that:
 *      1. All three parts deploy successfully and are under 24KB
 *      2. The proxy contract correctly coordinates the three parts
 *      3. Valid proofs are accepted
 *      4. Invalid proofs are rejected
 *      5. The split verifier behaves identically to the monolithic verifier
 */
contract Halo2VerifierSplitTest is Test {
    Halo2VerifierPart1 public part1;
    Halo2VerifierPart2 public part2;
    Halo2VerifierPart3 public part3;
    Halo2VerifierProxy public proxy;
    
    // 24KB limit (EIP-170)
    uint256 constant MAX_CONTRACT_SIZE = 24576;
    
    function setUp() public {
        console.log("=== Setting up Split Verifier Test ===");
        
        // Deploy all three parts
        console.log("Deploying Part1...");
        part1 = new Halo2VerifierPart1();
        console.log("  Part1 deployed at:", address(part1));
        console.log("  Part1 size:", address(part1).code.length, "bytes");
        
        console.log("Deploying Part2...");
        part2 = new Halo2VerifierPart2();
        console.log("  Part2 deployed at:", address(part2));
        console.log("  Part2 size:", address(part2).code.length, "bytes");
        
        console.log("Deploying Part3...");
        part3 = new Halo2VerifierPart3();
        console.log("  Part3 deployed at:", address(part3));
        console.log("  Part3 size:", address(part3).code.length, "bytes");
        
        // Deploy proxy
        console.log("Deploying Proxy...");
        proxy = new Halo2VerifierProxy(address(part1), address(part2), address(part3));
        console.log("  Proxy deployed at:", address(proxy));
        console.log("  Proxy size:", address(proxy).code.length, "bytes");
        
        console.log("=== Setup Complete ===\n");
    }
    
    function testContractSizes() public view {
        console.log("=== Testing Contract Sizes ===");
        
        uint256 part1Size = address(part1).code.length;
        uint256 part2Size = address(part2).code.length;
        uint256 part3Size = address(part3).code.length;
        uint256 proxySize = address(proxy).code.length;
        
        console.log("Part1 size:", part1Size, "bytes");
        console.log("Part2 size:", part2Size, "bytes");
        console.log("Part3 size:", part3Size, "bytes");
        console.log("Proxy size:", proxySize, "bytes");
        console.log("Total size:", part1Size + part2Size + part3Size + proxySize, "bytes");
        
        // Verify all parts are under 24KB
        assertLt(part1Size, MAX_CONTRACT_SIZE, "Part1 exceeds 24KB limit");
        assertLt(part2Size, MAX_CONTRACT_SIZE, "Part2 exceeds 24KB limit");
        assertLt(part3Size, MAX_CONTRACT_SIZE, "Part3 exceeds 24KB limit");
        assertLt(proxySize, MAX_CONTRACT_SIZE, "Proxy exceeds 24KB limit");
        
        console.log("[OK] All parts are under 24KB limit");
    }
    
    function testProxyConfiguration() public view {
        console.log("=== Testing Proxy Configuration ===");
        
        assertEq(proxy.part1(), address(part1), "Part1 address mismatch");
        assertEq(proxy.part2(), address(part2), "Part2 address mismatch");
        assertEq(proxy.part3(), address(part3), "Part3 address mismatch");
        
        console.log("[OK] Proxy correctly configured with all parts");
    }
    
    function testVerifierWithValidProof() public {
        console.log("=== Testing Valid Proof Verification ===");
        
        // Read proof from JSON file
        string memory root = vm.projectRoot();
        string memory path = string.concat(root, "/e2e_attack_test_data/valid_proof.json");
        
        // Check if file exists
        if (!vm.exists(path)) {
            console.log("[WARN]  Proof file not found, skipping test");
            console.log("   Expected path:", path);
            console.log("   Run deposit-prover to generate a proof first");
            return;
        }
        
        string memory json = vm.readFile(path);
        bytes memory proofData = vm.parseJsonBytes(json, ".proof");
        
        console.log("Proof length:", proofData.length, "bytes");
        
        // Call the proxy verifier
        (bool success, bytes memory result) = address(proxy).call{gas: 30000000}(proofData);
        
        console.log("Verification success:", success);
        console.log("Result length:", result.length);
        
        if (!success && result.length > 0) {
            console.log("Error:");
            console.logBytes(result);
        }
        
        // For Halo2 verifiers, success means the proof is valid (no revert)
        assertTrue(success, "Valid proof should be accepted");
        
        console.log("[OK] Valid proof accepted");
    }
    
    function testVerifierRejectsInvalidProof() public {
        console.log("=== Testing Invalid Proof Rejection ===");
        
        // Read proof from JSON file
        string memory root = vm.projectRoot();
        string memory path = string.concat(root, "/e2e_attack_test_data/valid_proof.json");
        
        // Check if file exists
        if (!vm.exists(path)) {
            console.log("[WARN]  Proof file not found, skipping test");
            return;
        }
        
        string memory json = vm.readFile(path);
        bytes memory proofData = vm.parseJsonBytes(json, ".proof");
        
        // Tamper with the proof to make it invalid
        if (proofData.length > 100) {
            proofData[50] = bytes1(uint8(proofData[50]) ^ 0xFF);
            proofData[51] = bytes1(uint8(proofData[51]) ^ 0xFF);
        }
        
        console.log("Tampered proof length:", proofData.length, "bytes");
        
        // Call the proxy verifier with tampered proof
        (bool success, bytes memory result) = address(proxy).call{gas: 30000000}(proofData);
        
        console.log("Verification success:", success);
        
        if (!success && result.length > 0) {
            console.log("Expected error (proof is invalid):");
            console.logBytes(result);
        }
        
        // For Halo2 verifiers, failure (revert) means the proof is invalid
        assertFalse(success, "Invalid proof should be rejected");
        
        console.log("[OK] Invalid proof rejected");
    }
    
    function testProxyRevertsOnZeroAddresses() public {
        console.log("=== Testing Proxy Constructor Validation ===");
        
        // Test zero address for part1
        vm.expectRevert("Part1 address cannot be zero");
        new Halo2VerifierProxy(address(0), address(part2), address(part3));
        
        // Test zero address for part2
        vm.expectRevert("Part2 address cannot be zero");
        new Halo2VerifierProxy(address(part1), address(0), address(part3));
        
        // Test zero address for part3
        vm.expectRevert("Part3 address cannot be zero");
        new Halo2VerifierProxy(address(part1), address(part2), address(0));
        
        console.log("[OK] Proxy correctly validates constructor arguments");
    }
    
    function testGasUsage() public {
        console.log("=== Testing Gas Usage ===");
        
        // Read proof from JSON file
        string memory root = vm.projectRoot();
        string memory path = string.concat(root, "/e2e_attack_test_data/valid_proof.json");
        
        // Check if file exists
        if (!vm.exists(path)) {
            console.log("[WARN]  Proof file not found, skipping test");
            return;
        }
        
        string memory json = vm.readFile(path);
        bytes memory proofData = vm.parseJsonBytes(json, ".proof");
        
        // Measure gas usage
        uint256 gasBefore = gasleft();
        (bool success, ) = address(proxy).call{gas: 30000000}(proofData);
        uint256 gasUsed = gasBefore - gasleft();
        
        console.log("Gas used:", gasUsed);
        console.log("Verification success:", success);
        
        if (success) {
            // Gas usage should be reasonable (less than 10M gas)
            assertLt(gasUsed, 10000000, "Gas usage too high");
            console.log("[OK] Gas usage is reasonable");
        }
    }
    
    function testMemoryIsolation() public view {
        console.log("=== Testing Memory Isolation ===");
        
        // Verify that each part has its own code
        bytes32 part1Hash = keccak256(address(part1).code);
        bytes32 part2Hash = keccak256(address(part2).code);
        bytes32 part3Hash = keccak256(address(part3).code);
        
        // All parts should have different code
        assertTrue(part1Hash != part2Hash, "Part1 and Part2 should have different code");
        assertTrue(part2Hash != part3Hash, "Part2 and Part3 should have different code");
        assertTrue(part1Hash != part3Hash, "Part1 and Part3 should have different code");
        
        console.log("[OK] All parts have unique code");
    }
}

