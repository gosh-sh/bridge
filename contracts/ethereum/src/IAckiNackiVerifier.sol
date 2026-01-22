// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IAckiNackiVerifier
 * @notice Interface for ZK proof verification
 * @dev This interface will be implemented by the actual Halo2 verifier contract
 */
interface IAckiNackiVerifier {
    /**
     * @notice Verify a withdrawal proof
     * @param proof The ZK proof bytes
     * @param publicInputs Array of public inputs for the proof
     * @return bool True if the proof is valid, false otherwise
     */
    function verifyWithdrawalProof(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external view returns (bool);

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure returns (uint256);
}

