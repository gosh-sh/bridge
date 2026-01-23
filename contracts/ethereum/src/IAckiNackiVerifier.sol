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
     * @param publicInputs Array of public inputs/outputs for the proof
     *                     Expected format: [nullifier, recipient, amount, root]
     *                     Note: nullifier is a public OUTPUT computed inside the circuit from private inputs
     * @return isValid True if the proof is valid, false otherwise
     * @return nullifier The nullifier (public output from the circuit)
     */
    function verifyWithdrawalProof(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external returns (bool isValid, bytes32 nullifier);

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier (4: nullifier, recipient, amount, root)
     */
    function getPublicInputsCount() external pure returns (uint256);
}

