// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/AckiNackiBridge.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";
import "../../src/IBridgeWithdrawalFinalVerifier.sol";
import "../../src/IBridgeMultiHopVerifier.sol";

/// @title VerifyBlockConfigLib
/// @notice Test-only helper for assembling `AckiNackiBridge.VerifyBlockConfig`
///         and `AckiNackiBridge.BridgeWithdrawConfig` literals without
///         re-typing every field. The deposit / AAVE / verifyBlock suites
///         only need *disabled* configs — pass `disabled()` /
///         `disabledWithdraw()` and the corresponding entrypoint reverts
///         with `VerifyBlockDisabled` / `WithdrawByProofBundleDisabled`.
library VerifyBlockConfigLib {
    /// @notice Build an all-zero `VerifyBlockConfig` (verifyBlock disabled).
    function disabled() internal pure returns (AckiNackiBridge.VerifyBlockConfig memory) {
        return AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(address(0)),
            fallbackVerifier: IFallbackVerifier(address(0)),
            layerHashesVerifier: ILayerHashesMovementVerifier(address(0)),
            genesisBkSetCommitment: 0,
            genesisPrevMaxLevelLayerHash: 0,
            genesisLastSeenBlockSeqNo: 0
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
            genesisPrevMaxLevelLayerHash: genesisPrevAnchor,
            genesisLastSeenBlockSeqNo: 0
        });
    }

    /// @notice All-zero `BridgeWithdrawConfig` — Circuit 4
    ///         `withdrawByProofBundle` disabled.
    function disabledWithdraw()
        internal
        pure
        returns (AckiNackiBridge.BridgeWithdrawConfig memory)
    {
        return AckiNackiBridge.BridgeWithdrawConfig({
            dappFr: 0,
            accFr: 0,
            altDstChainId: 0,
            altDstHostChainId: 0,
            altTokenId: 0,
            withdrawalFinalVerifier: IBridgeWithdrawalFinalVerifier(address(0)),
            multiHopVerifier: IBridgeMultiHopVerifier(address(0))
        });
    }

    /// @notice `BridgeWithdrawConfig` wired with the multi-thread FinalProof
    ///         verifier and the hop-chain verifier. Both verifiers must be
    ///         non-zero for the bundle path to be enabled; `accFr` must be
    ///         non-zero (`InvalidBridgeWithdrawalIdentity`); `dappFr` may be
    ///         zero on shellnet (zero `dapp_id` deployments).
    function withWithdrawBundle(
        IBridgeWithdrawalFinalVerifier finalVerifier,
        IBridgeMultiHopVerifier multiHopVerifier,
        uint256 dappFr,
        uint256 accFr
    ) internal pure returns (AckiNackiBridge.BridgeWithdrawConfig memory) {
        return AckiNackiBridge.BridgeWithdrawConfig({
            dappFr: dappFr,
            accFr: accFr,
            altDstChainId: 0,
            altDstHostChainId: 0,
            altTokenId: 0,
            withdrawalFinalVerifier: finalVerifier,
            multiHopVerifier: multiHopVerifier
        });
    }

    /// @notice Shellnet E2E wiring for the bundle path: logical
    ///         `altDstChainId` accepted only on `altDstHostChainId` (e.g.
    ///         Sepolia accepts AN proofs with `dstChainId = 1`).
    function withWithdrawBundleShellnet(
        IBridgeWithdrawalFinalVerifier finalVerifier,
        IBridgeMultiHopVerifier multiHopVerifier,
        uint256 dappFr,
        uint256 accFr,
        uint256 altDstChainId,
        uint256 altDstHostChainId,
        uint256 altTokenId
    ) internal pure returns (AckiNackiBridge.BridgeWithdrawConfig memory) {
        return AckiNackiBridge.BridgeWithdrawConfig({
            dappFr: dappFr,
            accFr: accFr,
            altDstChainId: altDstChainId,
            altDstHostChainId: altDstHostChainId,
            altTokenId: altTokenId,
            withdrawalFinalVerifier: finalVerifier,
            multiHopVerifier: multiHopVerifier
        });
    }
}
