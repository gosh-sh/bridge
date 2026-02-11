// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";

/**
 * @title DummyVerifier
 * @notice ⚠️ TEST VERIFIER - NOT SECURE FOR PRODUCTION
 * @dev This is a test-only verifier that accepts any valid proof format.
 *      Replace with Groth16DepositVerifier for mainnet deployment.
 *
 *      Public inputs:
 *        - depositId: Unique deposit identifier from the event
 *        - sender: Original depositor address from the event
 *        - amount: Deposit amount in wei from the event
 *        - contractAddress: Bridge contract address that emitted the event
 *        - blockHashHigh: High 128 bits of the block hash
 *        - blockHashLow: Low 128 bits of the block hash
 */
contract DummyVerifier is IAckiNackiVerifier {
    // Expected number of public inputs for deposit proof
    // Public inputs: [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
    uint256 private constant PUBLIC_INPUTS_COUNT = 6;

    /**
     * @notice Verify a withdrawal proof (TEST VERSION - basic format checks only)
     * @param proof The proof bytes (format not verified in test mode)
     * @param publicInputs Array of public inputs [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
     * @return isValid True if the proof passes basic format checks
     * @return depositId The deposit ID from public inputs
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        pure
        override
        returns (bool isValid, bytes32 depositId)
    {
        // Validate proof is not empty
        if (proof.length == 0) {
            return (false, bytes32(0));
        }

        // Validate public inputs count
        if (publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }

        // Extract depositId
        bytes32 depositIdValue = bytes32(publicInputs[0]);

        // Validate essential public inputs are non-zero
        if (publicInputs[1] == 0 || publicInputs[2] == 0 || publicInputs[3] == 0) {
            return (false, bytes32(0));
        }

        // TEST MODE: Accept any proof with valid format
        return (true, depositIdValue);
    }

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}
