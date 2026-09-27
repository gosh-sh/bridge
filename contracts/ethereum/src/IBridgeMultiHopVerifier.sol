// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeMultiHopVerifier
/// @notice Bridge-side interface for verifying `BridgeMultiHopProof`
///         (Rust: `bridge_event_prove_circuit::multi_hop_proof`). Each snark
///         proves a segment of the L7 cross-thread hop chain; the on-chain
///         gate in `withdrawByProofBundle` verifies as many hops as it takes
///         to walk `xBlockId → yBlockId`.
///
///         Public-input layout is two field elements:
///           `[0] hopStartBlockId` — Fr of the segment's first block_id
///           `[1] hopEndBlockId`   — Fr of the segment's last  block_id
///
///         Same-thread claims pass zero hops; the bundle gate then requires
///         `xBlockId == yBlockId` at the FinalProof level.
interface IBridgeMultiHopVerifier {
    /// @notice 2 public inputs of `BridgeMultiHopProof`.
    struct MultiHopPublicInputs {
        uint256 hopStartBlockId;
        uint256 hopEndBlockId;
    }

    /// @notice Verify a `BridgeMultiHopProof` (2-instance) proof.
    /// @param proof SHPLONK proof bytes.
    /// @param pub Public-input slots — the 2 field elements.
    /// @return isValid true on a passing proof; reverts inside the underlying
    ///         SHPLONK verifier are caught and surfaced as `false`.
    function verifyMultiHop(
        bytes calldata proof,
        MultiHopPublicInputs calldata pub
    ) external view returns (bool isValid);
}
