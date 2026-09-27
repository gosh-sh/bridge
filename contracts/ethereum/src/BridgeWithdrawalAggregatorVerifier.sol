// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeWithdrawalVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @title BridgeWithdrawalAggregatorVerifier
/// @notice R15 adapter: verifies Circuit 4 (`BridgeEventFinalProof`,
///         multi-thread) via SHPLONK aggregator Yul verifier.
/// @dev `proof` calldata = `instances (12 acc + 13 inner) ‖ snark_proof`.
///      Re-exposed inner PIs at indices 12..=24 (12,13,…,24 inclusive
///      — thirteen slots) must match `pub`. The previous single-thread
///      variant re-exposed 11 slots; every deployment tied to the old
///      layout must redeploy this adapter alongside the rotated Circuit-4
///      verifying key.
contract BridgeWithdrawalAggregatorVerifier is
    IBridgeWithdrawalVerifier,
    ShplonkAggregatorVerifierBase
{
    uint256 private constant NUM_INNER = 13;

    constructor(address _shplonkVerifier) ShplonkAggregatorVerifierBase(_shplonkVerifier) { }

    /// @inheritdoc IBridgeWithdrawalVerifier
    function verifyWithdrawal(bytes calldata proof, WithdrawalPublicInputs calldata pub)
        external
        view
        override
        returns (bool isValid)
    {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER) * 32) {
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

        return _verifyShplonk(proof);
    }
}
