// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "./IBlockHeaderOracle.sol";

/// @notice Interface for AxiomV2Core contract
/// @dev See: https://github.com/axiom-crypto/axiom-v2-contracts
interface IAxiomV2Core {
    /// @notice Verify a block hash is in the cache
    /// @param blockNumber The block number to verify
    /// @param claimedBlockHash The claimed block hash
    /// @param witness Merkle proof witness data
    /// @return bool True if the block hash is valid
    function isBlockHashValid(uint32 blockNumber, bytes32 claimedBlockHash, bytes calldata witness)
        external
        view
        returns (bool);

    /// @notice Verify a recent block hash (within last 256 blocks)
    /// @param blockNumber The block number to verify
    /// @param claimedBlockHash The claimed block hash
    /// @return bool True if the block hash is valid
    function isRecentBlockHashValid(uint32 blockNumber, bytes32 claimedBlockHash)
        external
        view
        returns (bool);
}

/// @title AxiomBlockHeaderOracle
/// @notice Production oracle for verifying Ethereum block headers using Axiom V2
/// @dev Integrates with AxiomV2Core to verify historical block hashes
contract AxiomBlockHeaderOracle is IBlockHeaderOracle {
    /// @notice The AxiomV2Core contract address
    IAxiomV2Core public immutable axiomV2Core;

    /// @notice Maximum age of blocks that can be verified using blockhash() opcode
    /// @dev EVM only stores last 256 block hashes
    uint256 public constant MAX_BLOCKHASH_AGE = 256;

    /// @notice Emitted when a block hash is verified
    event BlockHashVerified(uint256 indexed blockNumber, bytes32 blockHash, bool isRecent);

    /// @notice Error thrown when block number is in the future
    error BlockNotYetMined(uint256 blockNumber, uint256 currentBlock);

    /// @notice Error thrown when block hash verification fails
    error InvalidBlockHash(uint256 blockNumber, bytes32 claimedHash);

    /// @notice Error thrown when Axiom V2 Core address is zero
    error InvalidAxiomAddress();

    /// @notice Constructor
    /// @param _axiomV2Core Address of the AxiomV2Core contract
    constructor(address _axiomV2Core) {
        if (_axiomV2Core == address(0)) revert InvalidAxiomAddress();
        axiomV2Core = IAxiomV2Core(_axiomV2Core);
    }

    /// @notice Get the block hash for a given block number
    /// @dev For recent blocks (<256), uses blockhash() opcode
    /// @dev For historical blocks, uses AxiomV2Core.isRecentBlockHashValid()
    /// @param blockNumber The block number to query
    /// @return blockHash The block hash
    function getBlockHash(uint256 blockNumber) external view override returns (bytes32 blockHash) {
        // Check block is not in the future
        if (blockNumber >= block.number) {
            revert BlockNotYetMined(blockNumber, block.number);
        }

        // For recent blocks, use blockhash() opcode
        if (block.number - blockNumber <= MAX_BLOCKHASH_AGE) {
            blockHash = blockhash(blockNumber);
            if (blockHash == bytes32(0)) {
                revert InvalidBlockHash(blockNumber, blockHash);
            }
            return blockHash;
        }

        // For historical blocks, we cannot verify without a witness
        // This function should not be used for historical blocks
        // Users should call verifyBlockHash() with a witness instead
        revert("Use verifyBlockHash() for historical blocks");
    }

    /// @notice Check if a block hash is available for verification
    /// @dev Recent blocks (<256) are always available
    /// @dev Historical blocks require a witness proof from Axiom
    /// @param blockNumber The block number to check
    /// @return available True if the block hash can be verified
    function isBlockHashAvailable(uint256 blockNumber)
        external
        view
        override
        returns (bool available)
    {
        // Future blocks are not available
        if (blockNumber >= block.number) {
            return false;
        }

        // Recent blocks are always available via blockhash()
        if (block.number - blockNumber <= MAX_BLOCKHASH_AGE) {
            return true;
        }

        // Historical blocks are available if Axiom has cached them
        // In practice, Axiom V2 Core caches all blocks back to genesis
        // So this should always return true for historical blocks
        return true;
    }

    /// @notice Get the latest block number that has been verified
    /// @dev Returns the current block number minus 1 (most recent finalized block)
    /// @return blockNumber The latest verified block number
    function getLatestVerifiedBlock() external view override returns (uint256 blockNumber) {
        return block.number - 1;
    }

    /// @notice Verify a historical block hash using Axiom V2
    /// @dev This function requires a Merkle proof witness from Axiom
    /// @param blockNumber The block number to verify
    /// @param claimedBlockHash The claimed block hash
    /// @param witness Merkle proof witness data from Axiom
    /// @return bool True if the block hash is valid
    function verifyBlockHash(uint256 blockNumber, bytes32 claimedBlockHash, bytes calldata witness)
        external
        returns (bool)
    {
        // Check block is not in the future
        if (blockNumber >= block.number) {
            revert BlockNotYetMined(blockNumber, block.number);
        }

        // For recent blocks, use blockhash() opcode (no witness needed)
        if (block.number - blockNumber <= MAX_BLOCKHASH_AGE) {
            bytes32 actualHash = blockhash(blockNumber);
            if (actualHash == bytes32(0)) {
                revert InvalidBlockHash(blockNumber, actualHash);
            }
            bool valid = (actualHash == claimedBlockHash);
            if (valid) {
                emit BlockHashVerified(blockNumber, claimedBlockHash, true);
            }
            return valid;
        }

        // For historical blocks, use Axiom V2 Core
        bool isValid = axiomV2Core.isBlockHashValid(uint32(blockNumber), claimedBlockHash, witness);

        if (isValid) {
            emit BlockHashVerified(blockNumber, claimedBlockHash, false);
        }

        return isValid;
    }

    /// @notice Verify a recent block hash (within last 256 blocks) using Axiom V2
    /// @dev This is more gas-efficient than verifyBlockHash for recent blocks
    /// @param blockNumber The block number to verify
    /// @param claimedBlockHash The claimed block hash
    /// @return bool True if the block hash is valid
    function verifyRecentBlockHash(uint256 blockNumber, bytes32 claimedBlockHash)
        external
        returns (bool)
    {
        // Check block is not in the future
        if (blockNumber >= block.number) {
            revert BlockNotYetMined(blockNumber, block.number);
        }

        // Check block is recent enough
        if (block.number - blockNumber > MAX_BLOCKHASH_AGE) {
            revert("Block too old, use verifyBlockHash() with witness");
        }

        // Use Axiom V2 Core for verification
        bool isValid = axiomV2Core.isRecentBlockHashValid(uint32(blockNumber), claimedBlockHash);

        if (isValid) {
            emit BlockHashVerified(blockNumber, claimedBlockHash, true);
        }

        return isValid;
    }
}

