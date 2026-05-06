// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IPrimaryVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Primary** attestation
///         proofs (Circuit 1A). A Primary attestation is the first-round
///         attestation produced when ≥2/3 of the BK set has signed; it is the
///         common-case finalization path. Public-input layout mirrors Circuit 1B
///         exactly so the bridge can route either attestation type through a
///         shared decoder.
/// @dev Public input layout (4 BN254 Fr elements, matching the Halo2 circuit's
///      `expose_public` order):
///      [0] envelopeHash         keccak/SHA-256 root of the 8-leaf envelope tree, reduced mod Fr
///      [1] bkSetCommitment      Poseidon commitment to the active BK set
///      [2] blockSeqNo           AN block sequence number being attested
///      [3] lastSeenBlockSeqNo   monotonic anchor: the contract's currently stored seqno
interface IPrimaryVerifier {
    /// @notice Verify a Circuit 1A (Primary attestation) proof.
    /// @param proof 256-byte Groth16 proof (8 × uint256, gnark MarshalSolidity layout)
    /// @param envelopeHash field-encoded 8-leaf SHA-256 envelope root
    /// @param bkSetCommitment Poseidon commitment to the BK set
    /// @param blockSeqNo AN block sequence number
    /// @param lastSeenBlockSeqNo previously verified block's seqno (must be strictly less than `blockSeqNo`)
    /// @return isValid true on a passing proof, false otherwise. Reverts are
    ///         caught and surfaced as `false` so the calling bridge contract
    ///         can branch cleanly without try/catch.
    function verifyPrimaryAttestation(
        bytes calldata proof,
        uint256 envelopeHash,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view returns (bool isValid);
}
