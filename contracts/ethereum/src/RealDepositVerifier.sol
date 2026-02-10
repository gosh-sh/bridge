// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";

/**
 * @title RealDepositVerifier
 * @notice Production verifier for deposit proofs using real Halo2 verification
 * @dev Wraps the generated Halo2Verifier contract to implement IAckiNackiVerifier interface
 */
contract RealDepositVerifier is IAckiNackiVerifier {
    // The real Halo2 verifier contract (deployed separately)
    address public immutable HALO2_VERIFIER;

    // Expected public inputs: [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promise_commit]
    // FIX BC-CIRCUIT-004: Updated to 7 to include promise_commit
    uint256 private constant PUBLIC_INPUTS_COUNT = 7;

    error InvalidVerifierAddress();
    error VerificationFailed();

    constructor(address _halo2Verifier) {
        if (_halo2Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        HALO2_VERIFIER = _halo2Verifier;
    }

    /**
     * @notice Verify a deposit proof using real Halo2 verification
     * @param proof The ZK proof bytes
     * @param publicInputs [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promise_commit]
     * @return isValid Whether the proof is valid
     * @return depositId The deposit ID from public inputs
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
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

        // Extract and validate depositId
        bytes32 depositIdValue = bytes32(publicInputs[0]);
        if (depositIdValue == bytes32(0) || publicInputs[1] == 0 || publicInputs[2] == 0 || publicInputs[3] == 0) {
            return (false, bytes32(0));
        }

        // Prepare calldata for Halo2 verifier
        // The Halo2Verifier expects: [instance_0, instance_1, ..., instance_6, proof_bytes...]
        bytes memory verifierCalldata = abi.encodePacked(
            bytes32(publicInputs[0]), // depositId
            bytes32(publicInputs[1]), // sender
            bytes32(publicInputs[2]), // amount
            bytes32(publicInputs[3]), // contractAddress
            bytes32(publicInputs[4]), // blockHashHigh
            bytes32(publicInputs[5]), // blockHashLow
            bytes32(publicInputs[6]), // promise_commit
            proof
        );

        // Call the Halo2 verifier
        // The verifier will check that the proof is valid for the given public inputs
        (bool success, bytes memory result) = HALO2_VERIFIER.call(verifierCalldata);

        if (!success) {
            return (false, bytes32(0));
        }

        // The Halo2 verifier returns true (0x01) if the proof is valid
        // Note: Some verifiers return empty bytes on success, others return 0x01
        if (result.length == 0) {
            // Empty return means success for some verifiers
            return (true, depositIdValue);
        } else if (result.length == 32) {
            // Check if the result is true (non-zero)
            bool verified = abi.decode(result, (bool));
            return (verified, depositIdValue);
        } else {
            // Unexpected return format
            return (false, bytes32(0));
        }
    }

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}

