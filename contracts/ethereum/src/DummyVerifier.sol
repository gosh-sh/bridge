// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./IAckiNackiVerifier.sol";

/**
 * @title DummyVerifier
 * @notice Dummy implementation of the verifier for testing purposes
 * @dev This contract accepts all proofs as valid. DO NOT USE IN PRODUCTION!
 */
contract DummyVerifier is IAckiNackiVerifier {
    // Expected number of public inputs for withdrawal proof
    // Public inputs: [nullifier, recipient, amount, root]
    uint256 private constant PUBLIC_INPUTS_COUNT = 4;

    /**
     * @notice Verify a withdrawal proof (dummy implementation)
     * @dev This dummy implementation performs basic validation but accepts all proofs
     * @param proof The ZK proof bytes
     * @param publicInputs Array of public inputs for the proof
     * @return bool Always returns true if basic validation passes
     */
    function verifyWithdrawalProof(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external pure override returns (bool) {
        // Basic validation: proof must not be empty
        if (proof.length == 0) {
            return false;
        }

        // Basic validation: must have correct number of public inputs
        if (publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return false;
        }

        // Basic validation: nullifier must not be zero
        if (publicInputs[0] == 0) {
            return false;
        }

        // Basic validation: recipient must not be zero
        if (publicInputs[1] == 0) {
            return false;
        }

        // Basic validation: amount must not be zero
        if (publicInputs[2] == 0) {
            return false;
        }

        // Basic validation: root must not be zero
        if (publicInputs[3] == 0) {
            return false;
        }

        // In a real implementation, this would verify the ZK proof
        // For testing, we accept all proofs that pass basic validation
        return true;
    }

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}

