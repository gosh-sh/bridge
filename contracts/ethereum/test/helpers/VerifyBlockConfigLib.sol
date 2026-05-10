// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/AckiNackiBridge.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";

/// @title VerifyBlockConfigLib
/// @notice Test-only helper for assembling `AckiNackiBridge.VerifyBlockConfig`
///         literals without typing 5 named fields each time. The deposit /
///         AAVE / withdraw test suites only need a *disabled* config — pass
///         `disabled()` and `verifyBlock` reverts with `VerifyBlockDisabled`,
///         mirroring how legacy deployments behaved before Phase 4.
library VerifyBlockConfigLib {
    /// @notice Build an all-zero `VerifyBlockConfig` (verifyBlock disabled).
    function disabled() internal pure returns (AckiNackiBridge.VerifyBlockConfig memory) {
        return AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0
        });
    }

    /// @notice Build a `VerifyBlockConfig` with the supplied verifier triple
    ///         and genesis anchors. Used by the Phase 4 verifyBlock suite.
    function with(
        IPrimaryVerifier primary,
        IFallbackVerifier fallback_,
        ILayerHashesMovementVerifier layerHashes,
        uint256 genesisBkSetCommitment,
        uint256 genesisPrevAnchor
    ) internal pure returns (AckiNackiBridge.VerifyBlockConfig memory) {
        return AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: primary,
            fallbackVerifier: fallback_,
            layerHashesVerifier: layerHashes,
            genesisBkSetCommitment: genesisBkSetCommitment,
            genesisPrevMaxLevelLayerHash: genesisPrevAnchor
        });
    }
}
