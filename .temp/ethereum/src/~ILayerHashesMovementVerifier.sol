// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title ILayerHashesMovementVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Layer Hashes
///         Movement** proofs (Circuit 2). Each accepted proof binds together:
///         - the AN block whose attestation was already verified by Circuit 1A/1B
///           (via the shared `blockId` public input);
///         - the new per-layer hash root values being committed on Ethereum;
///         - a Poseidon Merkle chain that anchors `prev_max_level_layer_hash`
///           into the chain currently stored on-chain.
///
/// @dev Public-input layout (14 BN254 Fr elements; matches the Halo2
///      circuit's `expose_public` order in
///      `historical-layer-hashes-movement-checker-circuit/src/circuit.rs`):
///      ```
///      [0]      blockId                   keccak/SHA-256 root of the 8-leaf
///                                         envelope tree, bound to Circuit 1A/1B
///      [1]      bkSetCommitment           Poseidon commitment to the active BK set
///      [2]      numLayers                 1..=10 active layers
///      [3..=12] layerHashes[0..10]        per-layer Poseidon Merkle roots; the
///                                         (numLayers - 1)-th entry is the chain
///                                         result, the rest are committed roots
///      [13]     prevMaxLevelLayerHash     Poseidon root of the previous-block
///                                         chain (anchor for delta proofs)
///      ```
interface ILayerHashesMovementVerifier {
    /// @notice Verify a Circuit 2 (Layer Hashes Movement) proof.
    /// @param proof 256-byte Groth16 proof (8 × uint256, gnark MarshalSolidity layout)
    /// @param blockId 32-byte AN block identifier (must equal the value carried by Circuit 1A/1B)
    /// @param bkSetCommitment Poseidon commitment to the BK set
    /// @param numLayers number of active layers (1..=10)
    /// @param layerHashes 10 layer-hash field elements; index ≥ numLayers must be 0
    /// @param prevMaxLevelLayerHash Poseidon root anchoring the previous chain
    /// @return isValid true on a passing proof; reverts in the underlying gnark
    ///         verifier are caught and surfaced as `false`.
    function verifyLayerHashesMovement(
        bytes calldata proof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 numLayers,
        uint256[10] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external view returns (bool isValid);
}
