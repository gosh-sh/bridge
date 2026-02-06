// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBlockHeaderOracle.sol";

/// @title MockBlockHeaderOracle
/// @notice Mock implementation of IBlockHeaderOracle for testing
/// @dev This is for TESTING ONLY - uses blockhash() which only works for last 256 blocks
///      In production, use a real oracle like Axiom or Herodotus
contract MockBlockHeaderOracle is IBlockHeaderOracle {
    /// @notice Get the block hash for a specific block number
    /// @param blockNumber The block number to query
    /// @return blockHash The block hash from blockhash()
    /// @dev Only works for the last 256 blocks. Returns 0 for older blocks.
    function getBlockHash(uint256 blockNumber) external view override returns (bytes32 blockHash) {
        // blockhash() only works for the last 256 blocks
        if (block.number - blockNumber > 256) {
            revert("Block too old (>256 blocks)");
        }

        blockHash = blockhash(blockNumber);

        // blockhash() returns 0 for invalid block numbers
        if (blockHash == bytes32(0)) {
            revert("Block hash not available");
        }

        return blockHash;
    }

    /// @notice Check if a block hash is available
    /// @param blockNumber The block number to check
    /// @return available True if within last 256 blocks
    function isBlockHashAvailable(uint256 blockNumber)
        external
        view
        override
        returns (bool available)
    {
        return block.number - blockNumber <= 256 && blockNumber < block.number;
    }

    /// @notice Get the latest verified block number
    /// @return blockNumber The previous block number
    function getLatestVerifiedBlock() external view override returns (uint256 blockNumber) {
        return block.number - 1;
    }
}

