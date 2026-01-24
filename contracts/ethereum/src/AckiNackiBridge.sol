// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";
import "poseidon-solidity/PoseidonT3.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain
/// @dev Uses incremental Merkle tree (Tornado Cash style) and ZK proofs
contract AckiNackiBridge {
    // Merkle tree parameters
    uint256 public constant TREE_HEIGHT = 20;
    uint256 public constant MAX_LEAVES = 2 ** TREE_HEIGHT;

    // Merkle tree state
    uint256 public nextIndex;
    mapping(uint256 => bytes32) public leaves;
    bytes32[TREE_HEIGHT] public filledSubtrees;
    bytes32[TREE_HEIGHT + 1] public zeroHashes;

    // Nullifier tracking (prevent double-spending)
    mapping(bytes32 => bool) public nullifiers;

    // Treasury
    uint256 public treasuryBalance;

    // Verifier contract
    IAckiNackiVerifier public verifier;
    
    // Events
    event Deposit(
        bytes32 indexed commitment,
        bytes32 indexed commitmentWithAmount,
        uint256 leafIndex,
        uint256 amount,
        uint256 timestamp
    );
    
    event Withdrawal(
        bytes32 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 timestamp
    );
    
    // Errors
    error TreeFull();
    error InvalidAmount();
    error NullifierAlreadyUsed();
    error InvalidProof();
    error InsufficientTreasury();
    error InvalidVerifier();

    constructor(address _verifier) {
        if (_verifier == address(0)) revert InvalidVerifier();
        verifier = IAckiNackiVerifier(_verifier);

        // Initialize zero hashes for the Merkle tree
        // zero_hashes[0] = hash(0)
        // zero_hashes[i] = hash(zero_hashes[i-1], zero_hashes[i-1])
        zeroHashes[0] = bytes32(0);
        for (uint256 i = 0; i < TREE_HEIGHT; i++) {
            zeroHashes[i + 1] = hashPair(zeroHashes[i], zeroHashes[i]);
            filledSubtrees[i] = zeroHashes[i];
        }
    }
    
    /// @notice Deposit tokens and add commitment to Merkle tree
    /// @param commitment Hash of (withdrawalHash, nullifier)
    /// @param amount Amount to deposit
    function deposit(bytes32 commitment, uint256 amount) external payable {
        if (msg.value != amount) revert InvalidAmount();
        if (amount == 0) revert InvalidAmount();
        if (nextIndex >= MAX_LEAVES) revert TreeFull();
        
        // Add to treasury
        treasuryBalance += amount;
        
        // Store leaf
        uint256 leafIndex = nextIndex;
        leaves[leafIndex] = commitment;
        
        // Update Merkle tree using incremental algorithm
        bytes32 currentHash = commitment;
        uint256 currentIndex = leafIndex;
        
        for (uint256 level = 0; level < TREE_HEIGHT; level++) {
            if (currentIndex % 2 == 0) {
                // Left node - store and wait for right sibling
                filledSubtrees[level] = currentHash;
                break;
            } else {
                // Right node - hash with left sibling
                bytes32 leftSibling = filledSubtrees[level];
                currentHash = hashPair(leftSibling, currentHash);
                currentIndex /= 2;
            }
        }
        
        // Compute commitment with amount
        bytes32 commitmentWithAmount = hashPair(commitment, bytes32(amount));
        
        nextIndex++;
        
        emit Deposit(commitment, commitmentWithAmount, leafIndex, amount, block.timestamp);
    }
    
    /// @notice Withdraw tokens using ZK proof
    /// @param recipient Address to receive tokens
    /// @param amount Amount to withdraw
    /// @param root Merkle root
    /// @param nullifier Nullifier (public output computed in circuit from private inputs)
    /// @param proof ZK proof (cryptographic proof data)
    /// @dev The ZK proof proves that the user knows private inputs (withdrawal_hash, nullifier_preimage)
    ///      such that Poseidon(withdrawal_hash, nullifier_preimage) = nullifier
    function withdraw(
        address payable recipient,
        uint256 amount,
        bytes32 root,
        bytes32 nullifier,
        bytes calldata proof
    ) external {
        // Verify the root matches current tree root
        bytes32 currentRoot = getRoot();
        if (root != currentRoot) revert InvalidProof();

        // Prepare public inputs for the verifier
        // Public inputs/outputs: [nullifier, recipient, amount, root]
        // Note: nullifier is a public OUTPUT computed inside the circuit from private inputs
        uint256[] memory publicInputs = new uint256[](4);
        publicInputs[0] = uint256(nullifier);
        publicInputs[1] = uint256(uint160(address(recipient)));
        publicInputs[2] = amount;
        publicInputs[3] = uint256(root);

        // Verify ZK proof
        // The proof proves the user knows private inputs that produce this nullifier
        (bool isValid, bytes32 verifiedNullifier) = verifier.verifyWithdrawalProof(proof, publicInputs);
        if (!isValid) revert InvalidProof();

        // Sanity check: verifier should return the same nullifier
        require(verifiedNullifier == nullifier, "Nullifier mismatch");

        // Check nullifier hasn't been used
        if (nullifiers[nullifier]) revert NullifierAlreadyUsed();

        // Check treasury has enough balance
        if (treasuryBalance < amount) revert InsufficientTreasury();
        
        // Mark nullifier as used
        nullifiers[nullifier] = true;
        
        // Update treasury
        treasuryBalance -= amount;
        
        // Transfer tokens
        recipient.transfer(amount);
        
        emit Withdrawal(nullifier, recipient, amount, block.timestamp);
    }
    
    /// @notice Get current Merkle root
    /// @return Current root of the Merkle tree
    function getRoot() public view returns (bytes32) {
        if (nextIndex == 0) {
            return zeroHashes[TREE_HEIGHT];
        }
        
        // Start from the last inserted leaf
        uint256 lastLeafIndex = nextIndex - 1;
        bytes32 currentHash = leaves[lastLeafIndex];
        uint256 index = lastLeafIndex;
        
        for (uint256 level = 0; level < TREE_HEIGHT; level++) {
            bool isRight = index % 2 == 1;
            
            if (isRight) {
                bytes32 leftSibling = filledSubtrees[level];
                currentHash = hashPair(leftSibling, currentHash);
            } else {
                bytes32 rightZero = zeroHashes[level];
                currentHash = hashPair(currentHash, rightZero);
            }
            
            index /= 2;
        }
        
        return currentHash;
    }
    
    /// @notice Hash two values together using Poseidon hash
    /// @dev Uses PoseidonT3 (width 3, rate 2) for ZK compatibility
    /// @param left Left value
    /// @param right Right value
    /// @return Hash of the pair
    function hashPair(bytes32 left, bytes32 right) public pure returns (bytes32) {
        // Convert bytes32 to uint256 for Poseidon
        uint256[2] memory inputs = [uint256(left), uint256(right)];
        // PoseidonT3.hash returns uint256, convert back to bytes32
        return bytes32(PoseidonT3.hash(inputs));
    }
    
    /// @notice Check if a nullifier has been used
    /// @param nullifier Nullifier to check
    /// @return True if nullifier has been used
    function isNullifierUsed(bytes32 nullifier) external view returns (bool) {
        return nullifiers[nullifier];
    }
    
    /// @notice Get the number of leaves in the tree
    /// @return Number of leaves
    function getLeafCount() external view returns (uint256) {
        return nextIndex;
    }
    
    /// @notice Get a leaf at a specific index
    /// @param index Leaf index
    /// @return Leaf value
    function getLeaf(uint256 index) external view returns (bytes32) {
        require(index < nextIndex, "Invalid index");
        return leaves[index];
    }
}

