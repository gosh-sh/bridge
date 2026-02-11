// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBlockHeaderOracle.sol";

/// @title MockBlockHeaderOracle
/// @notice Mock implementation of IBlockHeaderOracle for testing
/// @dev This is for TESTING ONLY.
///      Supports two modes:
///      1. Fallback to blockhash() for recent blocks (works in Foundry tests)
///      2. Manually set block hashes via setBlockHash() for E2E tests on post-merge chains
///         where blockhash() returns the beacon block root, not the execution layer hash
///      In production, use a real oracle like Axiom or Herodotus
contract MockBlockHeaderOracle is IBlockHeaderOracle {
    /// @notice Owner who can set block hashes
    address public immutable owner;

    /// @notice Manually set block hashes (execution layer hashes)
    mapping(uint256 => bytes32) public storedBlockHashes;

    constructor() {
        owner = msg.sender;
    }

    /// @notice Set the execution layer block hash for a given block number
    /// @param blockNumber The block number
    /// @param blockHash The execution layer block hash (keccak256 of block header RLP)
    function setBlockHash(uint256 blockNumber, bytes32 blockHash) external {
        require(msg.sender == owner, "Only owner");
        storedBlockHashes[blockNumber] = blockHash;
    }

    /// @notice Get the block hash for a specific block number
    /// @param blockNumber The block number to query
    /// @return blockHash The block hash
    /// @dev First checks stored hashes, then falls back to blockhash()
    function getBlockHash(uint256 blockNumber) external view override returns (bytes32 blockHash) {
        // First check if we have a manually stored hash
        blockHash = storedBlockHashes[blockNumber];
        if (blockHash != bytes32(0)) {
            return blockHash;
        }

        // Fallback to blockhash() for recent blocks (works in Foundry tests)
        if (block.number - blockNumber <= 256) {
            blockHash = blockhash(blockNumber);
            if (blockHash != bytes32(0)) {
                return blockHash;
            }
        }

        revert("Block hash not available");
    }

    /// @notice Check if a block hash is available
    /// @param blockNumber The block number to check
    /// @return available True if hash is available
    function isBlockHashAvailable(uint256 blockNumber)
        external
        view
        override
        returns (bool available)
    {
        if (storedBlockHashes[blockNumber] != bytes32(0)) {
            return true;
        }
        return block.number - blockNumber <= 256 && blockNumber < block.number;
    }

    /// @notice Get the latest verified block number
    /// @return blockNumber The previous block number
    function getLatestVerifiedBlock() external view override returns (uint256 blockNumber) {
        return block.number - 1;
    }
}

