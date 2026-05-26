// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IBridgeWithdrawalVerifier.sol";

/// @title MockBridgeWithdrawalVerifier
/// @notice Configurable mock for `IBridgeWithdrawalVerifier` (Circuit 4
///         single-final-root layout — 10 public inputs). Used by the
///         `AckiNackiBridge.withdrawByProof` tests to exercise the entrypoint
///         and the `_nullifiers` / payout plumbing without the (still
///         R15-stubbed) halo2 → gnark pipeline for Circuit 4.
///
/// @dev Two modes:
///      - **Loose** (default): returns `shouldAccept` regardless of the
///        forwarded args.
///      - **Strict public inputs**: when `useStrictPub == true`, the mock
///        also requires every field of `WithdrawalPublicInputs` to equal
///        the stored expected struct. Used to assert that
///        `withdrawByProof` correctly forwards the immutable bridge
///        identity and the caller-supplied event fields.
contract MockBridgeWithdrawalVerifier is IBridgeWithdrawalVerifier {
    bool public shouldAccept;

    bool public useStrictPub;
    WithdrawalPublicInputs private _expectedPub;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function setExpectedPub(WithdrawalPublicInputs calldata expected) external {
        _expectedPub = expected;
        useStrictPub = true;
    }

    function clearStrictPub() external {
        useStrictPub = false;
    }

    function verifyWithdrawal(
        bytes calldata, /* proof */
        WithdrawalPublicInputs calldata pub
    )
        external
        view
        override
        returns (bool)
    {
        if (!shouldAccept) return false;

        if (useStrictPub) {
            if (
                pub.tokenId != _expectedPub.tokenId || pub.amount != _expectedPub.amount
                    || pub.recipientHi != _expectedPub.recipientHi
                    || pub.recipientLo != _expectedPub.recipientLo
                    || pub.dstChainId != _expectedPub.dstChainId
                    || pub.senderAccFr != _expectedPub.senderAccFr
                    || pub.dappFr != _expectedPub.dappFr || pub.accFr != _expectedPub.accFr
                    || pub.nullifier != _expectedPub.nullifier
                    || pub.finalRoot != _expectedPub.finalRoot
            ) {
                return false;
            }
        }

        return true;
    }
}
