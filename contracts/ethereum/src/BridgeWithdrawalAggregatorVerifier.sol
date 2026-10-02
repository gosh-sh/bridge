// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeWithdrawalFinalVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @notice R15 adapter: verifies Circuit 4 (`BridgeEventFinalProof`,
///         multi-thread) via SHPLONK aggregator Yul verifier.
/// @dev `proof` calldata = `instances (12 acc + 13 inner + 1 vk_digest) ‖ snark_proof`.
///      Re-exposed inner PIs at indices 12..=24 must match `pub`; the tail
///      slot at index 25 must equal the immutable `vkDigest` set at deploy
///      time. This adapter implements `IBridgeWithdrawalFinalVerifier`; the
///      pre-bundle 11-slot variant is gone from the bridge and every
///      deployment must redeploy this adapter alongside the Circuit-4
///      verifying key.
contract BridgeWithdrawalAggregatorVerifier is
    IBridgeWithdrawalFinalVerifier,
    ShplonkAggregatorVerifierBase
{
    uint256 private constant NUM_INNER = 13;

    constructor(address _shplonkVerifier, bytes32 _vkDigest)
        ShplonkAggregatorVerifierBase(_shplonkVerifier, _vkDigest)
    { }

    /// @inheritdoc IBridgeWithdrawalFinalVerifier
    function verifyWithdrawalFinal(
        bytes calldata proof,
        WithdrawalFinalPublicInputs calldata pub
    ) external view override returns (bool isValid) {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER + 1) * 32) {
            return false;
        }

        if (_readInstance(proof, 12) != pub.tokenId) return false;
        if (_readInstance(proof, 13) != pub.amount) return false;
        if (_readInstance(proof, 14) != pub.recipientHi) return false;
        if (_readInstance(proof, 15) != pub.recipientLo) return false;
        if (_readInstance(proof, 16) != pub.dstChainId) return false;
        if (_readInstance(proof, 17) != pub.senderAccFr) return false;
        if (_readInstance(proof, 18) != pub.dappFr) return false;
        if (_readInstance(proof, 19) != pub.accFr) return false;
        if (_readInstance(proof, 20) != pub.nullifier) return false;
        if (_readInstance(proof, 21) != pub.finalRoot) return false;
        if (_readInstance(proof, 22) != pub.anchorLayer) return false;
        if (_readInstance(proof, 23) != pub.xBlockId) return false;
        if (_readInstance(proof, 24) != pub.yBlockId) return false;
        if (_readInstance(proof, NUM_ACCUMULATOR_INSTANCES + NUM_INNER) != uint256(vkDigest)) {
            return false;
        }

        return _verifyShplonk(proof);
    }
}
