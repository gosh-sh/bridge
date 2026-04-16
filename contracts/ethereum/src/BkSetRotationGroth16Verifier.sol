// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBkSetRotationGroth16Verifier
/// @notice Interface for the gnark-generated Groth16 verifier for BK set rotation proofs.
/// @dev The generated contract will have verifyProof(uint256[8], uint256[2]).
///      Reverts on invalid proof; returns normally on valid proof.
interface IBkSetRotationGroth16Verifier {
    function verifyProof(
        uint256[8] calldata proof,
        uint256[2] calldata input
    ) external view;
}
