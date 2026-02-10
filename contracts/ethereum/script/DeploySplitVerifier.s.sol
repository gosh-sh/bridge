// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

import "forge-std/Script.sol";
import "../src/Halo2VerifierPart1.sol";
import "../src/Halo2VerifierPart2.sol";
import "../src/Halo2VerifierPart3.sol";
import "../src/Halo2VerifierProxy.sol";
import "../src/RealDepositVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/AckiNackiBridge.sol";

/**
 * @title DeploySplitVerifier
 * @notice Deployment script for the split Halo2 verifier and bridge contracts
 * @dev This script deploys:
 *      1. Halo2VerifierPart1 (13KB)
 *      2. Halo2VerifierPart2 (13KB)
 *      3. Halo2VerifierPart3 (20KB)
 *      4. Halo2VerifierProxy (coordinates the three parts)
 *      5. RealDepositVerifier (wrapper that implements IAckiNackiVerifier)
 *      6. MockBlockHeaderOracle
 *      7. AckiNackiBridge
 * 
 * Usage:
 *   forge script script/DeploySplitVerifier.s.sol:DeploySplitVerifier \
 *     --rpc-url $SEPOLIA_RPC_URL \
 *     --broadcast \
 *     --verify
 */
contract DeploySplitVerifier is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        
        vm.startBroadcast(deployerPrivateKey);
        
        console.log("=== Deploying Split Halo2 Verifier ===");
        console.log("Deployer:", vm.addr(deployerPrivateKey));
        
        // Step 1: Deploy Part 1
        console.log("\n[1/7] Deploying Halo2VerifierPart1...");
        Halo2VerifierPart1 part1 = new Halo2VerifierPart1();
        console.log("  Part1 deployed at:", address(part1));
        console.log("  Part1 size:", address(part1).code.length, "bytes");
        
        // Step 2: Deploy Part 2
        console.log("\n[2/7] Deploying Halo2VerifierPart2...");
        Halo2VerifierPart2 part2 = new Halo2VerifierPart2();
        console.log("  Part2 deployed at:", address(part2));
        console.log("  Part2 size:", address(part2).code.length, "bytes");
        
        // Step 3: Deploy Part 3
        console.log("\n[3/7] Deploying Halo2VerifierPart3...");
        Halo2VerifierPart3 part3 = new Halo2VerifierPart3();
        console.log("  Part3 deployed at:", address(part3));
        console.log("  Part3 size:", address(part3).code.length, "bytes");
        
        // Step 4: Deploy Proxy
        console.log("\n[4/7] Deploying Halo2VerifierProxy...");
        Halo2VerifierProxy proxy = new Halo2VerifierProxy(
            address(part1),
            address(part2),
            address(part3)
        );
        console.log("  Proxy deployed at:", address(proxy));
        console.log("  Proxy size:", address(proxy).code.length, "bytes");
        
        // Step 5: Deploy RealDepositVerifier wrapper
        console.log("\n[5/7] Deploying RealDepositVerifier...");
        RealDepositVerifier verifier = new RealDepositVerifier(address(proxy));
        console.log("  RealDepositVerifier deployed at:", address(verifier));
        
        // Step 6: Deploy MockBlockHeaderOracle
        console.log("\n[6/7] Deploying MockBlockHeaderOracle...");
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        console.log("  MockBlockHeaderOracle deployed at:", address(oracle));
        
        // Step 7: Deploy AckiNackiBridge
        console.log("\n[7/7] Deploying AckiNackiBridge...");
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(verifier),
            address(oracle)
        );
        console.log("  AckiNackiBridge deployed at:", address(bridge));
        
        vm.stopBroadcast();
        
        // Print summary
        console.log("\n=== Deployment Summary ===");
        console.log("Halo2VerifierPart1:", address(part1));
        console.log("Halo2VerifierPart2:", address(part2));
        console.log("Halo2VerifierPart3:", address(part3));
        console.log("Halo2VerifierProxy:", address(proxy));
        console.log("RealDepositVerifier:", address(verifier));
        console.log("MockBlockHeaderOracle:", address(oracle));
        console.log("AckiNackiBridge:", address(bridge));
        
        console.log("\n=== Contract Sizes ===");
        console.log("Part1:", address(part1).code.length, "bytes");
        console.log("Part2:", address(part2).code.length, "bytes");
        console.log("Part3:", address(part3).code.length, "bytes");
        console.log("Proxy:", address(proxy).code.length, "bytes");
        console.log("Total verifier size:", 
            address(part1).code.length + 
            address(part2).code.length + 
            address(part3).code.length + 
            address(proxy).code.length, 
            "bytes");
        
        console.log("\n=== Next Steps ===");
        console.log("1. Verify contracts on Etherscan");
        console.log("2. Update e2e_test_data/deployment.json with new addresses");
        console.log("3. Run E2E tests to verify the split verifier works correctly");
    }
}

