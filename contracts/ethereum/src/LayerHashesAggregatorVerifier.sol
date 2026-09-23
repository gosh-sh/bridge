// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./ILayerHashesMovementVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @title LayerHashesAggregatorVerifier
/// @notice R15 SHPLONK adapter for Circuit 2 (14 public inputs + inner-VK digest).
/// @dev Calldata layout: `instances (12 acc + 14 inner + 1 vk_digest) ‖ snark_proof`.
contract LayerHashesAggregatorVerifier is
    ILayerHashesMovementVerifier,
    ShplonkAggregatorVerifierBase
{
    uint256 private constant NUM_INNER = 14;

    constructor(address _shplonkVerifier, bytes32 _vkDigest)
        ShplonkAggregatorVerifierBase(_shplonkVerifier, _vkDigest)
    { }

    function verifyLayerHashesMovement(
        bytes calldata proof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 numLayers,
        uint256[10] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external view override returns (bool isValid) {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER + 1) * 32) {
            return false;
        }
        if (_readInstance(proof, 12) != blockId) return false;
        if (_readInstance(proof, 13) != bkSetCommitment) return false;
        if (_readInstance(proof, 14) != numLayers) return false;
        for (uint256 i = 0; i < 10; i++) {
            if (_readInstance(proof, 15 + i) != layerHashes[i]) return false;
        }
        if (_readInstance(proof, 25) != prevMaxLevelLayerHash) return false;
        if (_readInstance(proof, NUM_ACCUMULATOR_INSTANCES + NUM_INNER) != uint256(vkDigest)) {
            return false;
        }
        return _verifyShplonk(proof);
    }
}
