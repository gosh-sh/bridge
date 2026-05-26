// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/AckiNackiBridge.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";

/// @title VerifyBlockConfigLib
/// @notice Test-only helper for assembling `AckiNackiBridge.VerifyBlockConfig`
///         and `AckiNackiBridge.BridgeWithdrawConfig` literals without
///         re-typing every field. The deposit / AAVE / verifyBlock suites
///         only need *disabled* configs — pass `disabled()` /
///         `disabledWithdraw()` and the corresponding entrypoint reverts
///         with `VerifyBlockDisabled` / `WithdrawByProofDisabled`.
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

    /// @notice All-zero `BridgeWithdrawConfig` — Circuit 4 `withdrawByProof`
    ///         disabled.
    function disabledWithdraw()
        internal
        pure
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0)),
            dappFr: 0,
            accFr: 0
        });
    }

    /// @notice `BridgeWithdrawConfig` wired with an explicit verifier and
    ///         AN-side `(dappFr, accFr)` identity. Both Fr values must be
    ///         non-zero when the verifier is non-zero (enforced by the
    ///         constructor — `InvalidBridgeWithdrawalIdentity`).
    function withWithdraw(IBridgeWithdrawalVerifier verifier, uint256 dappFr, uint256 accFr)
        internal
        pure
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: verifier,
            dappFr: dappFr,
            accFr: accFr
        });
    }
}
