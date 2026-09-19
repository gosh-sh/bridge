// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IFallbackVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Fallback** attestation
///         proofs (Circuit 1B). A Fallback attestation is the second-round
///         attestation produced when the Primary path doesn't reach quorum.
/// @dev Public input layout (4 BN254 Fr elements, matching the Halo2 circuit's
///      `expose_public` order):
///      [0] blockId              32-byte AN block identifier hash, reduced mod Fr
///      [1] bkSetCommitment      Poseidon commitment to the active BK set
///      [2] blockSeqNo           AN block sequence number being attested
///      [3] lastSeenBlockSeqNo   last_seen the attestation was proven against
///                               (`block_seq_no > last_seen`, in-circuit)
interface IFallbackVerifier {
    /// @notice Verify a Circuit 1B (Fallback attestation) proof.
    /// @param proof SHPLONK proof bytes (Halo2 KZG aggregator calldata: instances ‖ proof)
    /// @param blockId 32-byte AN block identifier
    /// @param bkSetCommitment Poseidon commitment to the BK set
    /// @param blockSeqNo AN block sequence number
    /// @param lastSeenBlockSeqNo previously verified block's seqno (must be strictly less than `blockSeqNo`)
    /// @return isValid true on a passing proof, false otherwise. Reverts are
    ///         caught and surfaced as `false` so the calling bridge contract
    ///         can branch cleanly without try/catch.
    function verifyFallbackAttestation(
        bytes calldata proof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view returns (bool isValid);
}
