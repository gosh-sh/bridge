// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeMultiHopVerifier.sol";
import "./ShplonkAggregatorVerifierBase.sol";

/// @title BridgeMultiHopAggregatorVerifier
/// @notice R15 adapter: verifies `BridgeMultiHopProof` (Rust
///         `bridge_event_prove_circuit::multi_hop_proof`) via a SHPLONK
///         aggregator Yul verifier.
/// @dev `proof` calldata = `instances (12 acc + 2 inner) ‖ snark_proof`.
///      Re-exposed inner PIs at indices 12..=13 must match `pub`.
contract BridgeMultiHopAggregatorVerifier is
    IBridgeMultiHopVerifier,
    ShplonkAggregatorVerifierBase
{
    uint256 private constant NUM_INNER = 2;

    constructor(address _shplonkVerifier) ShplonkAggregatorVerifierBase(_shplonkVerifier) { }

    /// @inheritdoc IBridgeMultiHopVerifier
    function verifyMultiHop(bytes calldata proof, MultiHopPublicInputs calldata pub)
        external
        view
        override
        returns (bool isValid)
    {
        if (proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER) * 32) {
            return false;
        }

        if (_readInstance(proof, 12) != pub.hopStartBlockId) return false;
        if (_readInstance(proof, 13) != pub.hopEndBlockId) return false;

        return _verifyShplonk(proof);
    }
}
