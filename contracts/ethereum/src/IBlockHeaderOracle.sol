// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBlockHeaderOracle
/// @notice Interface for verifying Ethereum block headers
/// @dev This oracle provides verified block hashes from Ethereum mainnet/testnet
///      Implementations can use:
///      - Axiom (https://www.axiom.xyz/) - ZK-proven block headers
///      - Herodotus (https://www.herodotus.dev/) - Storage proofs
///      - Custom light client implementation
interface IBlockHeaderOracle {
    /// @notice Get the block hash for a specific block number
    /// @param blockNumber The block number to query
    /// @return blockHash The verified block hash (keccak256 of block header RLP)
    /// @dev Reverts if the block number is not available or not verified
    function getBlockHash(uint256 blockNumber) external view returns (bytes32 blockHash);

    /// @notice Check if a block hash is available and verified
    /// @param blockNumber The block number to check
    /// @return available True if the block hash is available
    function isBlockHashAvailable(uint256 blockNumber) external view returns (bool available);

    /// @notice Get the latest verified block number
    /// @return blockNumber The latest block number with verified hash
    function getLatestVerifiedBlock() external view returns (uint256 blockNumber);
}

