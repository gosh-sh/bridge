// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AxiomBlockHeaderOracle.sol";

/// @title DeployAxiomOracle
/// @notice Deployment script for AxiomBlockHeaderOracle
/// @dev Run with: forge script script/DeployAxiomOracle.s.sol --rpc-url $RPC_URL --broadcast
contract DeployAxiomOracle is Script {
    // Axiom V2 Core addresses (official deployments)
    // Source: https://docs.axiom.xyz/docs/developer-resources/contract-addresses

    // Mainnet
    address constant AXIOM_V2_CORE_MAINNET = address(0); // TODO: Update with official address

    // Sepolia
    address constant AXIOM_V2_CORE_SEPOLIA = address(0); // TODO: Update with official address

    function run() external {
        // Get deployer private key from environment
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        // Get chain ID to determine which Axiom address to use
        uint256 chainId = block.chainid;

        address axiomV2Core;
        string memory network;

        if (chainId == 1) {
            // Ethereum Mainnet
            axiomV2Core = AXIOM_V2_CORE_MAINNET;
            network = "Mainnet";
        } else if (chainId == 11155111) {
            // Sepolia Testnet
            axiomV2Core = AXIOM_V2_CORE_SEPOLIA;
            network = "Sepolia";
        } else {
            revert("Unsupported chain ID");
        }

        console.log("Deploying AxiomBlockHeaderOracle on", network);
        console.log("Chain ID:", chainId);
        console.log("AxiomV2Core address:", axiomV2Core);

        // Start broadcasting transactions
        vm.startBroadcast(deployerPrivateKey);

        // Deploy oracle
        AxiomBlockHeaderOracle oracle = new AxiomBlockHeaderOracle(axiomV2Core);

        console.log("AxiomBlockHeaderOracle deployed at:", address(oracle));

        // Verify deployment
        require(address(oracle.axiomV2Core()) == axiomV2Core, "Axiom address mismatch");
        console.log("Deployment verified!");

        vm.stopBroadcast();

        // Print deployment summary
        console.log("\n=== Deployment Summary ===");
        console.log("Network:", network);
        console.log("Oracle:", address(oracle));
        console.log("AxiomV2Core:", axiomV2Core);
        console.log("\nNext steps:");
        console.log("1. Verify contract on Etherscan");
        console.log("2. Deploy bridge with oracle address");
    }
}

