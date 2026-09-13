// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IFallbackVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @title FallbackAggregatorVerifier
/// @notice R15 SHPLONK adapter for Circuit 1B (4 public inputs).
contract FallbackAggregatorVerifier is IFallbackVerifier, ShplonkAggregatorVerifierBase {
    uint256 private constant NUM_INNER = 4;

    constructor(address _shplonkVerifier) ShplonkAggregatorVerifierBase(_shplonkVerifier) {}

    function verifyFallbackAttestation(
        bytes calldata proof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view override returns (bool isValid) {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER) * 32) {
            return false;
        }
        if (_readInstance(proof, 12) != blockId) return false;
        if (_readInstance(proof, 13) != bkSetCommitment) return false;
        if (_readInstance(proof, 14) != blockSeqNo) return false;
        if (_readInstance(proof, 15) != lastSeenBlockSeqNo) return false;
        return _verifyShplonk(proof);
    }
}
