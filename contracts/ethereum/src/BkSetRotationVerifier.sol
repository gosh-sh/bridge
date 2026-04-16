// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBkSetRotationVerifier.sol";
import "./BkSetRotationGroth16Verifier.sol";

/// @title BkSetRotationVerifier
/// @notice Adapter that verifies BK set rotation proofs using a gnark-generated
///         Groth16 verifier.
/// @dev Public input layout (2 BN254 Fr elements, matching the Halo2 circuit):
///      [0] oldBkSetCommitment
///      [1] newBkSetCommitment
contract BkSetRotationVerifier is IBkSetRotationVerifier {
    IBkSetRotationGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = IBkSetRotationGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc IBkSetRotationVerifier
    function verifyRotation(
        bytes calldata proof,
        uint256 oldCommitment,
        uint256 newCommitment
    ) external view override returns (bool isValid) {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        uint256[2] memory circuitInputs;
        circuitInputs[0] = oldCommitment;
        circuitInputs[1] = newCommitment;

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
