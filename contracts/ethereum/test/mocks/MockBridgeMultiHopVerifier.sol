// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IBridgeMultiHopVerifier.sol";

/// @title MockBridgeMultiHopVerifier
/// @notice Configurable mock for `IBridgeMultiHopVerifier` (2-instance
///         `BridgeMultiHopProof`). The interface's `verifyMultiHop` is
///         `view`, so per-call sequencing isn't possible; tests that need to
///         reject a specific hop can register its `(hopStartBlockId,
///         hopEndBlockId)` pair with `setRejectFor` and the mock will refuse
///         that exact pair while `shouldAccept` still rules every other pair.
contract MockBridgeMultiHopVerifier is IBridgeMultiHopVerifier {
    bool public shouldAccept = true;

    mapping(bytes32 => bool) private _rejectPair;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    /// @notice Force `verifyMultiHop` to return `false` for the exact pair
    ///         `(hopStartBlockId, hopEndBlockId)`, regardless of `shouldAccept`.
    function setRejectFor(uint256 hopStartBlockId, uint256 hopEndBlockId) external {
        _rejectPair[keccak256(abi.encode(hopStartBlockId, hopEndBlockId))] = true;
    }

    function verifyMultiHop(
        bytes calldata, /* proof */
        MultiHopPublicInputs calldata pub
    ) external view override returns (bool) {
        bytes32 key = keccak256(abi.encode(pub.hopStartBlockId, pub.hopEndBlockId));
        if (_rejectPair[key]) return false;
        return shouldAccept;
    }
}
