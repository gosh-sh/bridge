// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/AckiNackiBridge.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";
import "../../src/IBridgeEventVerifier.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";

/// @title VerifyBlockConfigLib
/// @notice Test-only helper for assembling `AckiNackiBridge.VerifyBlockConfig`
///         and `AckiNackiBridge.BridgeEventConfig` literals without typing
///         every named field each time. The deposit / AAVE / withdraw test
///         suites only need *disabled* configs — pass `disabled()` /
///         `disabledBridgeEvent()` and the corresponding entrypoint reverts
///         with `VerifyBlockDisabled` / `VerifyEventDisabled`.
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

    /// @notice All-zero `BridgeEventConfig` — Circuit 4 verifyEvent disabled.
    function disabledBridgeEvent()
        internal
        pure
        returns (AckiNackiBridge.BridgeEventConfig memory)
    {
        return AckiNackiBridge.BridgeEventConfig({
            bridgeEventVerifier: IBridgeEventVerifier(address(0)), dappFr: 0, accFr: 0
        });
    }

    /// @notice `BridgeEventConfig` wired with an explicit verifier + AN-side
    ///         `(dappFr, accFr)` identity. Both Fr values must be non-zero
    ///         when the verifier is non-zero (enforced by the constructor).
    function withBridgeEvent(IBridgeEventVerifier verifier, uint256 dappFr, uint256 accFr)
        internal
        pure
        returns (AckiNackiBridge.BridgeEventConfig memory)
    {
        return AckiNackiBridge.BridgeEventConfig({
            bridgeEventVerifier: verifier, dappFr: dappFr, accFr: accFr
        });
    }

    /// @notice `BridgeEventConfig` with identity slots set but the Phase A
    ///         verifier zeroed. Useful when a deployment wants Phase B
    ///         (`withdrawByProof`) without exposing Phase A's `verifyEvent`.
    function bridgeEventIdentityOnly(uint256 dappFr, uint256 accFr)
        internal
        pure
        returns (AckiNackiBridge.BridgeEventConfig memory)
    {
        return AckiNackiBridge.BridgeEventConfig({
            bridgeEventVerifier: IBridgeEventVerifier(address(0)), dappFr: dappFr, accFr: accFr
        });
    }

    /// @notice All-zero `BridgeWithdrawConfig` — Circuit 4 v2 `withdrawByProof`
    ///         disabled.
    function disabledWithdraw()
        internal
        pure
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(address(0))
        });
    }

    /// @notice `BridgeWithdrawConfig` wired with an explicit verifier. The AN-
    ///         side identity is inherited from the paired `BridgeEventConfig`
    ///         (both Circuit 4 variants bind to the same TokenBridge), so
    ///         callers must ensure their `BridgeEventConfig` carries non-zero
    ///         `dappFr` / `accFr` (use `withBridgeEvent` or
    ///         `bridgeEventIdentityOnly`).
    function withWithdraw(IBridgeWithdrawalVerifier verifier)
        internal
        pure
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return AckiNackiBridge.BridgeWithdrawConfig({ bridgeWithdrawalVerifier: verifier });
    }
}
