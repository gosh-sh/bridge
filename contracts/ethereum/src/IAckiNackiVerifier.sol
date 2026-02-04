// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/**
 * @title IAckiNackiVerifier
 * @notice Interface for ZK proof verification
 * @dev This interface will be implemented by the actual Halo2 verifier contract
 */
interface IAckiNackiVerifier {
    /**
     * @notice Verify a withdrawal proof
     * @param proof The ZK proof bytes (cryptographic proof data)
     * @param publicInputs Array of public inputs for deposit proof verification
     *                     Format: [depositId, sender, amount, contractAddress]
     *                     - depositId: Unique deposit identifier (uint256)
     *                     - sender: Original depositor address (uint160 → uint256)
     *                     - amount: Deposit amount in wei (uint256)
     *                     - contractAddress: Bridge contract address (uint160 → uint256)
     * @return isValid True if the proof is valid, false otherwise
     * @return depositId The deposit ID from the proof (first public input)
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        returns (bool isValid, bytes32 depositId);

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier (4: depositId, sender, amount, contractAddress)
     */
    function getPublicInputsCount() external pure returns (uint256);
}

