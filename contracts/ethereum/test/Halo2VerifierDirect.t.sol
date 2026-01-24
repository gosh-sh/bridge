// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/console.sol";

contract Halo2VerifierDirectTest is Test {
    address public verifier;

    function setUp() public {
        // Read the bytecode from the compiled Yul file
        string memory bytecodeHex = vm.readFile("verifier_bytecode.hex");
        bytes memory bytecode = vm.parseBytes(bytecodeHex);
        
        address deployed;
        assembly {
            deployed := create(0, add(bytecode, 0x20), mload(bytecode))
        }
        require(deployed != address(0), "Failed to deploy verifier");
        verifier = deployed;
        console.log("Verifier deployed at:", verifier);
    }

    function testVerifierWithGeneratedProof() public {
        // Read proof from JSON file
        string memory root = vm.projectRoot();
        string memory path = string.concat(root, "/test/test_proof.json");
        string memory json = vm.readFile(path);
        
        bytes memory proof = vm.parseJsonBytes(json, ".proof");
        
        // Public inputs from the JSON
        uint256 nullifier = uint256(vm.parseJsonUint(json, ".publicInputs[0]"));
        uint256 recipient = uint256(vm.parseJsonUint(json, ".publicInputs[1]"));
        uint256 amount = uint256(vm.parseJsonUint(json, ".publicInputs[2]"));
        uint256 root_val = uint256(vm.parseJsonUint(json, ".publicInputs[3]"));

        console.log("Testing proof verification...");
        console.log("Proof length:", proof.length);
        console.log("Nullifier:", nullifier);
        console.log("Recipient:", recipient);
        console.log("Amount:", amount);
        console.log("Root:", root_val);

        // Call the verifier with public inputs + proof
        bytes memory calldata_ = abi.encodePacked(
            bytes32(nullifier),
            bytes32(recipient),
            bytes32(amount),
            bytes32(root_val),
            proof
        );

        (bool success, bytes memory result) = verifier.call{gas: 30000000}(calldata_);

        console.log("Call success:", success);
        console.log("Result length:", result.length);
        
        if (!success && result.length > 0) {
            console.logBytes(result);
        }
        
        assertTrue(success, "Verifier call should succeed");
        
        if (result.length > 0) {
            bool isValid = abi.decode(result, (bool));
            console.log("Proof is valid:", isValid);
            assertTrue(isValid, "Proof should be valid");
        }
    }
}

