// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/DummyVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";

/**
 * @title DeployTestBridge
 * @notice Deployment script for testing the deposit prover on Sepolia
 * @dev Deploys a simple test verifier that accepts any proof (for testing only!)
 */
contract DeployTestBridge is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        // Deploy a simple test verifier
        // For testing, we use a minimal verifier that just checks proof format
        // For production, use Groth16DepositVerifier via DeployRealBridge.s.sol
        TestDepositVerifier verifier = new TestDepositVerifier();
        console.log("TestDepositVerifier deployed at:", address(verifier));

        // Deploy mock block header oracle
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle deployed at:", address(oracle));

        // Deploy the bridge contract
        AckiNackiBridge bridge = new AckiNackiBridge(address(verifier), address(oracle));
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        // Print deployment info
        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Verifier:", address(verifier));
        console.log("Bridge:", address(bridge));
        console.log("\nTo make a test deposit:");
        console.log(
            "cast send",
            address(bridge),
            "\"deposit(uint256)\" 100000000000000000 --value 0.1ether --rpc-url $SEPOLIA_RPC_URL --private-key $PRIVATE_KEY"
        );
        console.log("\nSave these addresses for testing!");
    }
}

/**
 * @title TestDepositVerifier
 * @notice Simple test verifier for deposit proofs (TESTING ONLY - NOT SECURE!)
 * @dev This verifier accepts any proof with correct format for testing purposes
 *      In production, use Groth16DepositVerifier
 */
contract TestDepositVerifier is IAckiNackiVerifier {
    // Expected public inputs: [depositId, sender, amount, contractAddress]
    uint256 private constant PUBLIC_INPUTS_COUNT = 4;

    /**
     * @notice Verify a deposit proof (TEST VERSION - accepts any valid format)
     * @param proof The proof bytes (not verified in test mode)
     * @param publicInputs [depositId, sender, amount, contractAddress]
     * @return isValid Always true if inputs are valid format
     * @return depositId The deposit ID from public inputs
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        pure
        override
        returns (bool isValid, bytes32 depositId)
    {
        // Validate proof is not empty (minimum check)
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

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs (4 for deposit proofs)
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}

