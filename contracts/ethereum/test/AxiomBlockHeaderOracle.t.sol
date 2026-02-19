// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AxiomBlockHeaderOracle.sol";

/// @title AxiomBlockHeaderOracleTest
/// @notice Tests for AxiomBlockHeaderOracle contract
contract AxiomBlockHeaderOracleTest is Test {
    AxiomBlockHeaderOracle public oracle;
    MockAxiomV2Core public mockAxiom;

    address constant USER = address(0x1234);

    function setUp() public {
        // Deploy mock Axiom V2 Core
        mockAxiom = new MockAxiomV2Core();

        // Deploy oracle
        oracle = new AxiomBlockHeaderOracle(address(mockAxiom));
    }

    /// @notice Test constructor with valid address
    function test_Constructor() public {
        assertEq(address(oracle.axiomV2Core()), address(mockAxiom));
        assertEq(oracle.MAX_BLOCKHASH_AGE(), 256);
    }

    /// @notice Test constructor with zero address
    function test_Constructor_ZeroAddress() public {
        vm.expectRevert(AxiomBlockHeaderOracle.InvalidAxiomAddress.selector);
        new AxiomBlockHeaderOracle(address(0));
    }

    /// @notice Test getBlockHash for recent blocks
    function test_GetBlockHash_RecentBlock() public {
        // Mine some blocks
        vm.roll(1000);

        // Get block hash for recent block (within 256 blocks)
        uint256 blockNumber = block.number - 10;
        bytes32 expectedHash = blockhash(blockNumber);

        bytes32 actualHash = oracle.getBlockHash(blockNumber);

        assertEq(actualHash, expectedHash);
        assertTrue(actualHash != bytes32(0));
    }

    /// @notice Test getBlockHash for historical blocks (should revert)
    function test_GetBlockHash_HistoricalBlock() public {
        // Mine many blocks
        vm.roll(1000);

        // Try to get block hash for old block (>256 blocks ago)
        uint256 blockNumber = block.number - 300;

        vm.expectRevert("Use verifyBlockHash() for historical blocks");
        oracle.getBlockHash(blockNumber);
    }

    /// @notice Test getBlockHash for future blocks
    function test_GetBlockHash_FutureBlock() public {
        vm.roll(1000);

        uint256 futureBlock = block.number + 10;

        vm.expectRevert(
            abi.encodeWithSelector(
                AxiomBlockHeaderOracle.BlockNotYetMined.selector, futureBlock, block.number
            )
        );
        oracle.getBlockHash(futureBlock);
    }

    /// @notice Test isBlockHashAvailable for recent blocks
    function test_IsBlockHashAvailable_RecentBlock() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 10;
        assertTrue(oracle.isBlockHashAvailable(blockNumber));
    }

    /// @notice Test isBlockHashAvailable for historical blocks
    function test_IsBlockHashAvailable_HistoricalBlock() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 300;
        assertTrue(oracle.isBlockHashAvailable(blockNumber));
    }

    /// @notice Test isBlockHashAvailable for future blocks
    function test_IsBlockHashAvailable_FutureBlock() public {
        vm.roll(1000);

        uint256 futureBlock = block.number + 10;
        assertFalse(oracle.isBlockHashAvailable(futureBlock));
    }

    /// @notice Test getLatestVerifiedBlock
    function test_GetLatestVerifiedBlock() public {
        vm.roll(1000);

        uint256 latest = oracle.getLatestVerifiedBlock();
        assertEq(latest, block.number - 1);
    }

    /// @notice Test verifyBlockHash for recent blocks
    function test_VerifyBlockHash_RecentBlock() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 10;
        bytes32 blockHash = blockhash(blockNumber);

        // Verify with empty witness (not needed for recent blocks)
        bool isValid = oracle.verifyBlockHash(blockNumber, blockHash, "");

        assertTrue(isValid);
    }

    /// @notice Test verifyBlockHash for recent blocks with wrong hash
    function test_VerifyBlockHash_RecentBlock_WrongHash() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 10;
        bytes32 wrongHash = keccak256("wrong");

        // Verify with empty witness
        bool isValid = oracle.verifyBlockHash(blockNumber, wrongHash, "");

        assertFalse(isValid);
    }

    /// @notice Test verifyBlockHash for historical blocks
    function test_VerifyBlockHash_HistoricalBlock() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 300;
        bytes32 blockHash = keccak256(abi.encodePacked(blockNumber));

        // Set up mock Axiom to return true
        mockAxiom.setBlockHashValid(uint32(blockNumber), blockHash, true);

        // Verify with witness
        bytes memory witness = abi.encode("mock_witness");
        bool isValid = oracle.verifyBlockHash(blockNumber, blockHash, witness);

        assertTrue(isValid);
    }

    /// @notice Test verifyBlockHash for historical blocks with wrong hash
    function test_VerifyBlockHash_HistoricalBlock_WrongHash() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 300;
        bytes32 wrongHash = keccak256("wrong");

        // Set up mock Axiom to return false
        mockAxiom.setBlockHashValid(uint32(blockNumber), wrongHash, false);

        // Verify with witness
        bytes memory witness = abi.encode("mock_witness");
        bool isValid = oracle.verifyBlockHash(blockNumber, wrongHash, witness);

        assertFalse(isValid);
    }

    /// @notice Test verifyRecentBlockHash
    function test_VerifyRecentBlockHash() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 10;
        bytes32 blockHash = blockhash(blockNumber);

        // Set up mock Axiom to return true
        mockAxiom.setRecentBlockHashValid(uint32(blockNumber), blockHash, true);

        bool isValid = oracle.verifyRecentBlockHash(blockNumber, blockHash);

        assertTrue(isValid);
    }

    /// @notice Test verifyRecentBlockHash for old blocks
    function test_VerifyRecentBlockHash_OldBlock() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 300;
        bytes32 blockHash = keccak256(abi.encodePacked(blockNumber));

        vm.expectRevert("Block too old, use verifyBlockHash() with witness");
        oracle.verifyRecentBlockHash(blockNumber, blockHash);
    }

    /// @notice Test BlockHashVerified event emission
    function test_BlockHashVerified_Event() public {
        vm.roll(1000);

        uint256 blockNumber = block.number - 10;
        bytes32 blockHash = blockhash(blockNumber);

        // Expect event
        vm.expectEmit(true, false, false, true);
        emit BlockHashVerified(blockNumber, blockHash, true);

        oracle.verifyBlockHash(blockNumber, blockHash, "");
    }

    /// @notice Event declaration for testing
    event BlockHashVerified(uint256 indexed blockNumber, bytes32 blockHash, bool isRecent);
}

/// @title MockAxiomV2Core
/// @notice Mock implementation of AxiomV2Core for testing
contract MockAxiomV2Core {
    // Storage for mock responses
    mapping(uint32 => mapping(bytes32 => bool)) public blockHashValid;
    mapping(uint32 => mapping(bytes32 => bool)) public recentBlockHashValid;

    /// @notice Set mock response for isBlockHashValid
    function setBlockHashValid(uint32 blockNumber, bytes32 blockHash, bool valid) external {
        blockHashValid[blockNumber][blockHash] = valid;
    }

    /// @notice Set mock response for isRecentBlockHashValid
    function setRecentBlockHashValid(uint32 blockNumber, bytes32 blockHash, bool valid) external {
        recentBlockHashValid[blockNumber][blockHash] = valid;
    }

    /// @notice Mock implementation of isBlockHashValid
    function isBlockHashValid(
        uint32 blockNumber,
        bytes32 claimedBlockHash,
        bytes calldata /* witness */
    )
        external
        view
        returns (bool)
    {
        return blockHashValid[blockNumber][claimedBlockHash];
    }

    /// @notice Mock implementation of isRecentBlockHashValid
    function isRecentBlockHashValid(uint32 blockNumber, bytes32 claimedBlockHash)
        external
        view
        returns (bool)
    {
        return recentBlockHashValid[blockNumber][claimedBlockHash];
    }
}

