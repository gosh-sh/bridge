// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain
/// @dev Uses event-based proofs verified by ZK circuits (no on-chain Merkle tree)
contract AckiNackiBridge {
    // Nullifier tracking (prevent double-spending)
    mapping(bytes32 => bool) public nullifiers;

    // Treasury
    uint256 public treasuryBalance;

    // Verifier contract
    IAckiNackiVerifier public verifier;

    // Events
    event Deposit(
        bytes32 indexed depositHash,
        address indexed sender,
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
    error InvalidAmount();
    error NullifierAlreadyUsed();
    error InvalidProof();
    error InsufficientTreasury();
    error InvalidVerifier();

    constructor(address _verifier) {
        if (_verifier == address(0)) revert InvalidVerifier();
        verifier = IAckiNackiVerifier(_verifier);
    }
    
    /// @notice Deposit tokens to the bridge
    /// @param depositHash Hash identifying this deposit (e.g., hash of withdrawal secrets)
    /// @param amount Amount to deposit
    /// @dev Emits Deposit event which will be proven by ZK circuit for withdrawal
    function deposit(bytes32 depositHash, uint256 amount) external payable {
        if (msg.value != amount) revert InvalidAmount();
        if (amount == 0) revert InvalidAmount();

        // Add to treasury
        treasuryBalance += amount;

        // Emit event with all necessary data for ZK proof
        // The ZK circuit will prove this event was emitted by this contract
        emit Deposit(depositHash, msg.sender, amount, block.timestamp);
    }
    
    /// @notice Withdraw tokens using ZK proof of deposit event
    /// @param recipient Address to receive tokens
    /// @param amount Amount to withdraw
    /// @param nullifier Nullifier (prevents double-spending)
    /// @param proof ZK proof proving:
    ///              1. A Deposit event was emitted by this contract
    ///              2. The event contains the correct depositHash, amount, and sender
    ///              3. The prover knows the secrets that hash to depositHash
    ///              4. The nullifier is derived from those secrets
    /// @dev The ZK circuit verifies the Ethereum receipt trie to prove event emission
    function withdraw(
        address payable recipient,
        uint256 amount,
        bytes32 nullifier,
        bytes calldata proof
    ) external {
        // Prepare public inputs for the verifier
        // Public inputs: [nullifier, recipient, amount, contractAddress]
        uint256[] memory publicInputs = new uint256[](4);
        publicInputs[0] = uint256(nullifier);
        publicInputs[1] = uint256(uint160(address(recipient)));
        publicInputs[2] = amount;
        publicInputs[3] = uint256(uint160(address(this)));

        // Verify ZK proof
        // The proof verifies that a Deposit event was emitted and the nullifier is correct
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

    /// @notice Check if a nullifier has been used
    /// @param nullifier Nullifier to check
    /// @return True if nullifier has been used
    function isNullifierUsed(bytes32 nullifier) external view returns (bool) {
        return nullifiers[nullifier];
    }
}

