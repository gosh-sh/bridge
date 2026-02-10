// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/RealDepositVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/AckiNackiBridge.sol";

/**
 * @title DeployRealBridgeWithBytecode
 * @notice Deployment script for the real Acki-Nacki bridge with Halo2 verifier deployed from bytecode
 * @dev This script deploys the Halo2 verifier using raw bytecode (CREATE opcode) to bypass
 *      the Solidity compiler and the 24KB contract size limit.
 *
 * Usage:
 *   # Dry run (simulation)
 *   forge script script/DeployRealBridgeWithBytecode.s.sol
 *
 *   # Deploy to Sepolia testnet
 *   source .env && forge script script/DeployRealBridgeWithBytecode.s.sol \
 *     --rpc-url $SEPOLIA_RPC_URL \
 *     --broadcast \
 *     --verify \
 *     --etherscan-api-key $ETHERSCAN_API_KEY
 *
 * Note: The Halo2 verifier bytecode is 45KB which exceeds the 24KB mainnet limit,
 *       but testnets typically don't enforce this limit strictly.
 */
contract DeployRealBridgeWithBytecode is Script {
    function run() external {
        // Read the deployment bytecode from file
        string memory root = vm.projectRoot();
        string memory bytecodePath = string.concat(root, "/Halo2Verifier.bytecode.hex");
        string memory bytecodeHex = vm.readFile(bytecodePath);

        // Remove "0x" prefix if present
        if (bytes(bytecodeHex)[0] == "0" && bytes(bytecodeHex)[1] == "x") {
            bytecodeHex = substring(bytecodeHex, 2, bytes(bytecodeHex).length);
        }

        // Convert hex string to bytes
        bytes memory bytecode = vm.parseBytes(bytecodeHex);

        console.log("=== Deploying Real Acki-Nacki Bridge with Halo2 Verifier ===");
        console.log("Bytecode size:", bytecode.length, "bytes");
        console.log("Bytecode size:", bytecode.length / 1024, "KB");

        if (bytecode.length > 24576) {
            console.log("WARNING: Bytecode exceeds 24KB Ethereum contract size limit!");
            console.log("This deployment will only work on testnets or L2 networks.");
        }

        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        // 1. Deploy Halo2Verifier using CREATE opcode with raw bytecode
        console.log("\n1. Deploying Halo2Verifier from bytecode...");
        address halo2VerifierAddress;
        assembly {
            // CREATE(value, offset, size)
            // value: 0 (no ETH sent)
            // offset: add(bytecode, 0x20) (skip the length prefix)
            // size: mload(bytecode) (the actual bytecode length)
            halo2VerifierAddress := create(0, add(bytecode, 0x20), mload(bytecode))
        }

        require(halo2VerifierAddress != address(0), "Halo2Verifier deployment failed");
        console.log("   Halo2Verifier deployed at:", halo2VerifierAddress);

        // Verify the deployment by checking code size
        uint256 codeSize;
        assembly {
            codeSize := extcodesize(halo2VerifierAddress)
        }
        console.log("   Deployed code size:", codeSize, "bytes");
        require(codeSize > 0, "Halo2Verifier has no code");

        // 2. Deploy RealDepositVerifier wrapper
        console.log("\n2. Deploying RealDepositVerifier wrapper...");
        RealDepositVerifier verifier = new RealDepositVerifier(halo2VerifierAddress);
        console.log("   RealDepositVerifier deployed at:", address(verifier));

        // 3. Deploy MockBlockHeaderOracle
        console.log("\n3. Deploying MockBlockHeaderOracle...");
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("   MockBlockHeaderOracle deployed at:", address(oracle));

        // 4. Deploy AckiNackiBridge
        console.log("\n4. Deploying AckiNackiBridge...");
        AckiNackiBridge bridge = new AckiNackiBridge(address(verifier), address(oracle));
        console.log("   AckiNackiBridge deployed at:", address(bridge));

        vm.stopBroadcast();

        console.log("\n=== Deployment Summary ===");
        console.log("Halo2Verifier:         ", halo2VerifierAddress);
        console.log("RealDepositVerifier:   ", address(verifier));
        console.log("MockBlockHeaderOracle: ", address(oracle));
        console.log("AckiNackiBridge:       ", address(bridge));
        console.log("\nNext steps:");
        console.log("1. Test the verifier with a real proof");
        console.log("2. Run the negative E2E test to verify BC-CIRCUIT-004 fix");
        console.log("3. Update test_e2e_negative_attack.sh with the new bridge address");
    }

    // Helper function to extract substring
    function substring(string memory str, uint256 startIndex, uint256 endIndex) internal pure returns (string memory) {
        bytes memory strBytes = bytes(str);
        bytes memory result = new bytes(endIndex - startIndex);
        for (uint256 i = startIndex; i < endIndex; i++) {
            result[i - startIndex] = strBytes[i];
        }
        return string(result);
    }
}

