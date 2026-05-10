// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/ILayerHashesMovementVerifier.sol";

/// @title MockLayerHashesMovementVerifier
/// @notice Configurable mock for `ILayerHashesMovementVerifier`. Used by
///         Phase 5 relayer-loop tests to advance `AckiNackiBridge.verifyBlock`
///         across many sequential blocks without re-running Circuit 2 keygen
///         and proving for each one. The production
///         `LayerHashesMovementVerifier` is independently covered end-to-end
///         in `AckiNackiBridgeVerifyBlock.t.sol`.
///
/// @dev `ILayerHashesMovementVerifier.verifyLayerHashesMovement` is `view`,
///      so the mock keeps that mutability and reads `shouldAccept` from
///      storage. State changes are restricted to the explicit setter, called
///      from test setUp.
contract MockLayerHashesMovementVerifier is ILayerHashesMovementVerifier {
    /// @notice Toggle: when `true`, every call returns `true`; otherwise `false`.
    bool public shouldAccept;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyLayerHashesMovement(
        bytes calldata, /* proof */
        uint256, /* blockId */
        uint256, /* bkSetCommitment */
        uint256, /* numLayers */
        uint256[10] calldata, /* layerHashes */
        uint256 /* prevMaxLevelLayerHash */
    ) external view returns (bool) {
        return shouldAccept;
    }
}
