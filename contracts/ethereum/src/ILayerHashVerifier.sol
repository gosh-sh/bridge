// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

interface ILayerHashVerifier {
    /// @notice Verify a layer hash update proof from Acki Nacki.
    /// @param proof 256-byte Groth16 proof (8 × uint256 in EIP-197 format)
    /// @param bkSetCommitment Poseidon commitment to the Block Keeper set
    /// @param numLayers Number of active layers (1..MAX_LAYERS)
    /// @param layerHashes Array of MAX_LAYERS (10) layer hash Fr values
    /// @param prevMaxLevelLayerHash Previous top-level layer hash (chain anchor)
    /// @return isValid Whether the proof is valid
    function verifyLayerHashUpdate(
        bytes calldata proof,
        uint256 bkSetCommitment,
        uint256 numLayers,
        uint256[10] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external view returns (bool isValid);
}
