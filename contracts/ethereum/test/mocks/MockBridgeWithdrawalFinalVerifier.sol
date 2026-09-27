// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IBridgeWithdrawalFinalVerifier.sol";

/// @title MockBridgeWithdrawalFinalVerifier
/// @notice Configurable mock for `IBridgeWithdrawalFinalVerifier` (13-instance
///         `BridgeEventFinalProof`). Used by the `withdrawByProofBundle`
///         tests to exercise the on-chain acceptance gate — identity, chain
///         scoping, replay, canonicity, anchor window, hop-chain continuity
///         — without a real SHPLONK proof.
contract MockBridgeWithdrawalFinalVerifier is IBridgeWithdrawalFinalVerifier {
    bool public shouldAccept = true;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyWithdrawalFinal(
        bytes calldata, /* proof */
        WithdrawalFinalPublicInputs calldata /* pub */
    ) external view override returns (bool) {
        return shouldAccept;
    }
}
