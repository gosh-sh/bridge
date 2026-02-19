// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

import "./IAckiNackiVerifier.sol";
import "./Groth16Verifier.sol";

/**
 * @title Groth16DepositVerifier
 * @notice Production verifier for deposit proofs using Groth16 verification
 * @dev Wraps the gnark-generated Groth16Verifier to implement the IAckiNackiVerifier interface.
 *
 *      The Groth16 verifier expects:
 *        - proof: uint256[8] (A, B, C points in EIP-197 format, 256 bytes)
 *        - input: uint256[7] public inputs
 *
 *      The bridge passes:
 *        - proof bytes: Groth16 proof (256 bytes) + promise_commit (32 bytes) = 288 bytes
 *        - publicInputs: [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
 *
 *      This contract decodes the proof bytes, extracts promise_commit, assembles the 7 public
 *      inputs, and calls the Groth16Verifier.
 */
contract Groth16DepositVerifier is IAckiNackiVerifier {
    /// @notice The Groth16 verifier contract (gnark-generated)
    Groth16Verifier public immutable groth16Verifier;

    /// @notice Expected number of public inputs from the bridge (excluding promise_commit)
    uint256 private constant BRIDGE_PUBLIC_INPUTS_COUNT = 6;

    /// @notice Total public inputs for the Groth16 circuit (including promise_commit)
    uint256 private constant CIRCUIT_PUBLIC_INPUTS_COUNT = 7;

    /// @notice Size of an uncompressed Groth16 proof in bytes (8 uint256s = 256 bytes)
    uint256 private constant GROTH16_PROOF_SIZE = 256;

    /// @notice Size of promise_commit in bytes (1 uint256 = 32 bytes)
    uint256 private constant PROMISE_COMMIT_SIZE = 32;

    /// @notice Expected total proof bytes: Groth16 proof + promise_commit
    uint256 private constant EXPECTED_PROOF_BYTES = GROTH16_PROOF_SIZE + PROMISE_COMMIT_SIZE;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = Groth16Verifier(_groth16Verifier);
    }

    /**
     * @notice Verify a deposit proof using Groth16 verification
     * @param proof The proof bytes: Groth16 proof (256 bytes) + promise_commit (32 bytes)
     * @param publicInputs [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
     * @return isValid Whether the proof is valid
     * @return depositId The deposit ID from public inputs
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        view
        override
        returns (bool isValid, bytes32 depositId)
    {
        // Validate proof length: 256 bytes (Groth16 proof) + 32 bytes (promise_commit)
        if (proof.length != EXPECTED_PROOF_BYTES) {
            return (false, bytes32(0));
        }

        // Validate public inputs count from bridge
        if (publicInputs.length != BRIDGE_PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }

        // Extract depositId
        bytes32 depositIdValue = bytes32(publicInputs[0]);

        // Validate essential public inputs are non-zero
        // sender, amount, contractAddress must be non-zero
        if (publicInputs[1] == 0 || publicInputs[2] == 0 || publicInputs[3] == 0) {
            return (false, bytes32(0));
        }

        // Decode the Groth16 proof (8 uint256 values = A, B, C points)
        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        // Decode promise_commit from the last 32 bytes
        uint256 promiseCommit = uint256(bytes32(proof[GROTH16_PROOF_SIZE:EXPECTED_PROOF_BYTES]));

        // Assemble the 7 circuit public inputs
        uint256[7] memory circuitInputs;
        circuitInputs[0] = publicInputs[0]; // depositId
        circuitInputs[1] = publicInputs[1]; // sender
        circuitInputs[2] = publicInputs[2]; // amount
        circuitInputs[3] = publicInputs[3]; // contractAddress
        circuitInputs[4] = publicInputs[4]; // blockHashHigh
        circuitInputs[5] = publicInputs[5]; // blockHashLow
        circuitInputs[6] = promiseCommit; // promise_commit

        // Call the Groth16 verifier — it reverts on invalid proof
        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return (true, depositIdValue);
        } catch {
            return (false, bytes32(0));
        }
    }

    /**
     * @notice Get the expected number of public inputs (from bridge perspective)
     * @return uint256 The number of public inputs expected from the bridge (6)
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return BRIDGE_PUBLIC_INPUTS_COUNT;
    }
}

