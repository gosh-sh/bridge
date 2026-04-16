// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBkSetRotationVerifier
/// @notice Interface for verifying ZK proofs of BK set rotation.
/// @dev The proof demonstrates that the current Block Keeper committee
///      (identified by oldCommitment) BLS-attested a block containing
///      a transition to a new committee (identified by newCommitment).
interface IBkSetRotationVerifier {
    /// @notice Verify a BK set rotation proof.
    /// @param proof 256-byte Groth16 proof (8 x uint256 in EIP-197 format)
    /// @param oldCommitment Poseidon commitment to the current (old) BK set
    /// @param newCommitment Poseidon commitment to the new BK set
    /// @return isValid Whether the proof is valid
    function verifyRotation(bytes calldata proof, uint256 oldCommitment, uint256 newCommitment)
        external
        view
        returns (bool isValid);
}
