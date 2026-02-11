// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/Groth16DepositVerifier.sol";
import "../src/Groth16Verifier.sol";
import "../src/MockBlockHeaderOracle.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for production bridge with Groth16 verifier
 * @dev Deploys the Groth16 verifier (gnark-generated) and bridge contract
 */
contract DeployRealBridge is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        // Step 1: Deploy the Groth16Verifier contract (gnark-generated)
        console.log("Deploying Groth16Verifier...");
        Groth16Verifier groth16VerifierContract = new Groth16Verifier();
        address groth16VerifierAddr = address(groth16VerifierContract);
        console.log("Groth16Verifier deployed at:", groth16VerifierAddr);

        // Step 2: Deploy the Groth16DepositVerifier wrapper
        console.log("Deploying Groth16DepositVerifier wrapper...");
        Groth16DepositVerifier verifier = new Groth16DepositVerifier(groth16VerifierAddr);
        console.log("Groth16DepositVerifier deployed at:", address(verifier));

        // Step 3: Deploy mock block header oracle
        console.log("Deploying MockBlockHeaderOracle...");
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("MockBlockHeaderOracle deployed at:", address(oracle));

        // Step 4: Deploy the bridge contract
        console.log("Deploying AckiNackiBridge...");
        AckiNackiBridge bridge = new AckiNackiBridge(address(verifier), address(oracle));
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        // Print deployment summary
        console.log("\n=== Deployment Complete ===");
        console.log("Network: Sepolia");
        console.log("Groth16Verifier:", groth16VerifierAddr);
        console.log("Groth16DepositVerifier:", address(verifier));
        console.log("MockBlockHeaderOracle:", address(oracle));
        console.log("AckiNackiBridge:", address(bridge));
        console.log("\nTo make a deposit:");
        console.log(
            "cast send",
            address(bridge),
            "\"deposit()\" --value 0.001ether --rpc-url $SEPOLIA_RPC_URL --private-key $PRIVATE_KEY"
        );
        console.log("\nSave these addresses for testing!");

        // Save deployment info to JSON
        string memory deploymentJson = string(
            abi.encodePacked(
                "{\n",
                '  "groth16_verifier": "',
                vm.toString(groth16VerifierAddr),
                '",\n',
                '  "deposit_verifier": "',
                vm.toString(address(verifier)),
                '",\n',
                '  "oracle": "',
                vm.toString(address(oracle)),
                '",\n',
                '  "bridge": "',
                vm.toString(address(bridge)),
                '"\n',
                "}"
            )
        );

        vm.writeFile("deployment_real.json", deploymentJson);
        console.log("\nDeployment info saved to: deployment_real.json");
    }
}

