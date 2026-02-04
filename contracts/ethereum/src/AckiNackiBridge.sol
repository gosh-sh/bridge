// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain
/// @dev Uses event-based proofs verified by ZK circuits (no on-chain Merkle tree)
contract AckiNackiBridge {
    // Maximum deposit amount (prevents whale deposits)
    uint256 public constant MAX_DEPOSIT_AMOUNT = 100 ether;

    // Deposit tracking (prevent double-spending)
    mapping(uint256 => bool) public processedDeposits;

    // Deposit counter (unique ID for each deposit)
    // Note: uint256 max = 2^256 - 1 ≈ 10^77 deposits
    // At 1 deposit/second, would take 10^70 years to overflow
    uint256 public depositCounter;

    // Treasury
    uint256 public treasuryBalance;

    // Verifier contract
    IAckiNackiVerifier public verifier;

    // Events
    event Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp);

    event Withdrawal(uint256 indexed depositId, address indexed recipient, uint256 amount, uint256 timestamp);

    // Errors
    error InvalidAmount();
    error DepositTooLarge();
    error DepositAlreadyProcessed();
    error InvalidProof();
    error InsufficientTreasury();
    error InvalidVerifier();
    error InvalidRecipient();

    constructor(address _verifier) {
        if (_verifier == address(0)) revert InvalidVerifier();
        verifier = IAckiNackiVerifier(_verifier);
    }

    /// @notice Deposit tokens to the bridge
    /// @dev Emits Deposit event which will be proven by ZK circuit for withdrawal
    function deposit() external payable {
        if (msg.value == 0) revert InvalidAmount();
        if (msg.value > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();

        // Get unique deposit ID
        uint256 depositId = depositCounter++;

        // Add to treasury
        treasuryBalance += msg.value;

        // Emit event with all necessary data for ZK proof
        // The ZK circuit will prove this event was emitted by this contract
        emit Deposit(depositId, msg.sender, msg.value, block.timestamp);
    }

    /// @notice Withdraw tokens using ZK proof of deposit event
    /// @param recipient Address to receive tokens
    /// @param amount Amount to withdraw
    /// @param depositId Unique deposit ID (prevents double-spending)
    /// @param proof ZK proof proving:
    ///              1. A Deposit event was emitted by this contract
    ///              2. The event contains the correct depositId, amount, and sender
    ///              3. The sender matches the recipient
    /// @dev The ZK circuit verifies the Ethereum receipt trie to prove event emission
    function withdraw(address payable recipient, uint256 amount, uint256 depositId, bytes calldata proof) external {
        // Validate recipient address
        if (recipient == address(0)) revert InvalidRecipient();

        // Prepare public inputs for the verifier
        // Public inputs: [depositId, sender, amount, contractAddress]
        uint256[] memory publicInputs = new uint256[](4);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(address(recipient)));
        publicInputs[2] = amount;
        publicInputs[3] = uint256(uint160(address(this)));

        // Verify ZK proof
        // The proof verifies that a Deposit event was emitted with these parameters
        (bool isValid, bytes32 verifiedDepositId) = verifier.verifyWithdrawalProof(proof, publicInputs);
        if (!isValid) revert InvalidProof();

        // Sanity check: verifier should return the same depositId
        require(uint256(verifiedDepositId) == depositId, "DepositId mismatch");

        // Check deposit hasn't been processed
        if (processedDeposits[depositId]) revert DepositAlreadyProcessed();

        // Check treasury has enough balance
        if (treasuryBalance < amount) revert InsufficientTreasury();

        // Mark deposit as processed
        processedDeposits[depositId] = true;

        // Update treasury
        treasuryBalance -= amount;

        // Transfer tokens
        // Using transfer() instead of call() for defense in depth:
        // - 2300 gas limit prevents reentrancy attacks
        // - CEI pattern already implemented (state changes before transfer)
        // - Compatible with standard smart contract wallets
        recipient.transfer(amount);

        emit Withdrawal(depositId, recipient, amount, block.timestamp);
    }

    /// @notice Check if a deposit has been processed
    /// @param depositId Deposit ID to check
    /// @return True if deposit has been processed
    function isDepositProcessed(uint256 depositId) external view returns (bool) {
        return processedDeposits[depositId];
    }
}

