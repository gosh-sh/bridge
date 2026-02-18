// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/Groth16DepositVerifier.sol";
import "../src/Groth16Verifier.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/AxiomBlockHeaderOracle.sol";

/**
 * @title DeployRealBridge
 * @notice Deployment script for production bridge with Groth16 verifier
 * @dev Deploys the Groth16 verifier (gnark-generated) and bridge contract
 *
 * Oracle mode:
 *   - Set USE_AXIOM_ORACLE=true to deploy with AxiomBlockHeaderOracle (production)
 *   - Default (false) deploys MockBlockHeaderOracle (testing only)
 *
 * Axiom V2 Core addresses (from axiom-v2-contracts deployed.json):
 *   Mainnet: 0x69963768F8407dE501029680dE46945F838Fc98B
 *   Sepolia: 0x69963768F8407dE501029680dE46945F838Fc98B (mock, same address via CREATE3)
 */
contract DeployRealBridge is Script {
    // Axiom V2 Core addresses
    address constant AXIOM_V2_CORE_MAINNET = 0x69963768F8407dE501029680dE46945F838Fc98B;
    address constant AXIOM_V2_CORE_SEPOLIA = 0x69963768F8407dE501029680dE46945F838Fc98B;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        bool useAxiomOracle = vm.envOr("USE_AXIOM_ORACLE", false);

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

        // Step 3: Deploy block header oracle
        address oracleAddr;
        string memory oracleType;

        if (useAxiomOracle) {
            // Production: deploy AxiomBlockHeaderOracle
            address axiomV2Core;
            if (block.chainid == 1) {
                axiomV2Core = AXIOM_V2_CORE_MAINNET;
            } else if (block.chainid == 11155111) {
                axiomV2Core = AXIOM_V2_CORE_SEPOLIA;
            } else {
                revert(
                    "Axiom oracle not supported on this chain. Use USE_AXIOM_ORACLE=false for local testing."
                );
            }
            require(axiomV2Core != address(0), "Axiom V2 Core address not set");

            console.log("Deploying AxiomBlockHeaderOracle...");
            console.log("  AxiomV2Core:", axiomV2Core);
            AxiomBlockHeaderOracle axiomOracle = new AxiomBlockHeaderOracle(axiomV2Core);
            oracleAddr = address(axiomOracle);
            oracleType = "AxiomBlockHeaderOracle";
        } else {
            // Testing: deploy MockBlockHeaderOracle
            console.log("Deploying MockBlockHeaderOracle (TESTING ONLY)...");
            console.log(
                "  WARNING: Mock oracle is NOT trustless. Use USE_AXIOM_ORACLE=true for production."
            );
            MockBlockHeaderOracle mockOracle = new MockBlockHeaderOracle();
            oracleAddr = address(mockOracle);
            oracleType = "MockBlockHeaderOracle";
        }
        console.log(string(abi.encodePacked(oracleType, " deployed at:")), oracleAddr);

        // Step 4: Deploy the bridge contract
        console.log("Deploying AckiNackiBridge...");
        AckiNackiBridge bridge = new AckiNackiBridge(address(verifier), oracleAddr);
        console.log("AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        // Print deployment summary
        console.log("\n=== Deployment Complete ===");
        console.log("Oracle type:", oracleType);
        console.log("Groth16Verifier:", groth16VerifierAddr);
        console.log("Groth16DepositVerifier:", address(verifier));
        console.log("Oracle:", oracleAddr);
        console.log("AckiNackiBridge:", address(bridge));

        if (!useAxiomOracle) {
            console.log(
                "\n  ** WARNING: Using MockBlockHeaderOracle -- NOT suitable for production **"
            );
            console.log(
                "  ** Re-deploy with USE_AXIOM_ORACLE=true for trustless block hash verification **"
            );
        }

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
                vm.toString(oracleAddr),
                '",\n',
                '  "oracle_type": "',
                oracleType,
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

