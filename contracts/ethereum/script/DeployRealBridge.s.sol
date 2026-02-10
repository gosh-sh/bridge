// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/RealDepositVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/Halo2Verifier.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for production bridge with real Halo2 verifier
 * @dev Deploys the real Halo2 verifier and bridge contract
 */
contract DeployRealBridge is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        // Step 1: Deploy the Halo2Verifier contract
        console.log("Deploying Halo2Verifier...");

        Halo2Verifier halo2VerifierContract = new Halo2Verifier();
        address halo2Verifier = address(halo2VerifierContract);

        console.log("Halo2Verifier deployed at:", halo2Verifier);

        // Step 2: Deploy the RealDepositVerifier wrapper
        console.log("Deploying RealDepositVerifier wrapper...");
        RealDepositVerifier verifier = new RealDepositVerifier(halo2Verifier);
        console.log("RealDepositVerifier deployed at:", address(verifier));

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
        console.log("Halo2Verifier:", halo2Verifier);
        console.log("RealDepositVerifier:", address(verifier));
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
                '  "halo2_verifier": "',
                vm.toString(halo2Verifier),
                '",\n',
                '  "verifier_wrapper": "',
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

