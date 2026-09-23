// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IPrimaryVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @title PrimaryAggregatorVerifier
/// @notice R15 SHPLONK adapter for Circuit 1A (4 public inputs + inner-VK digest).
/// @dev Calldata layout: `instances (12 acc + 4 inner + 1 vk_digest) ‖ snark_proof`.
contract PrimaryAggregatorVerifier is IPrimaryVerifier, ShplonkAggregatorVerifierBase {
    uint256 private constant NUM_INNER = 4;

    constructor(address _shplonkVerifier, bytes32 _vkDigest)
        ShplonkAggregatorVerifierBase(_shplonkVerifier, _vkDigest)
    { }

    function verifyPrimaryAttestation(
        bytes calldata proof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view override returns (bool isValid) {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER + 1) * 32) {
            return false;
        }
        if (_readInstance(proof, 12) != blockId) return false;
        if (_readInstance(proof, 13) != bkSetCommitment) return false;
        if (_readInstance(proof, 14) != blockSeqNo) return false;
        if (_readInstance(proof, 15) != lastSeenBlockSeqNo) return false;
        if (_readInstance(proof, NUM_ACCUMULATOR_INSTANCES + NUM_INNER) != uint256(vkDigest)) {
            return false;
        }
        return _verifyShplonk(proof);
    }
}
