// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IBridgeWithdrawalVerifier.sol";

/// @title MockBridgeWithdrawalVerifier
/// @notice Configurable mock for `IBridgeWithdrawalVerifier`. Used by the
///         Phase B Circuit 4 v2 tests to exercise the `withdrawByProof`
///         entrypoint and the `_nullifiers` / payout plumbing without the
///         (yet-to-land) halo2 → gnark pipeline for Circuit 4 v2.
///
/// @dev Three modes:
///      - **Loose** (default): returns `shouldAccept` regardless of the
///        forwarded args.
///      - **Strict layer hashes**: if `useStrictLayerHashes == true`, the
///        mock also requires the forwarded `layerHashes` array to equal
///        `_expectedLayerHashes` byte-for-byte. Same trick as
///        `MockBridgeEventVerifier`: a passing test is itself the proof
///        that the bridge forwarded its own on-chain ring buffer snapshot.
///      - **Strict public inputs**: when `useStrictPub == true`, the mock
///        also requires every field of `WithdrawalPublicInputs` to equal
///        the stored expected struct. Used to assert that
///        `withdrawByProof` correctly forwards the immutable bridge
///        identity and the caller-supplied event fields.
contract MockBridgeWithdrawalVerifier is IBridgeWithdrawalVerifier {
    bool public shouldAccept;

    bool public useStrictLayerHashes;
    uint256[100] private _expectedLayerHashes;

    bool public useStrictPub;
    WithdrawalPublicInputs private _expectedPub;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function setExpectedLayerHashes(uint256[100] calldata expected) external {
        for (uint256 i = 0; i < 100; i++) {
            _expectedLayerHashes[i] = expected[i];
        }
        useStrictLayerHashes = true;
    }

    function clearStrictLayerHashes() external {
        useStrictLayerHashes = false;
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
        WithdrawalPublicInputs calldata pub,
        uint256[100] calldata layerHashes
    ) external view override returns (bool) {
        if (!shouldAccept) return false;

        if (useStrictPub) {
            if (
                pub.tokenId != _expectedPub.tokenId || pub.amount != _expectedPub.amount
                    || pub.recipientHi != _expectedPub.recipientHi
                    || pub.recipientLo != _expectedPub.recipientLo
                    || pub.dstChainId != _expectedPub.dstChainId
                    || pub.senderDappFr != _expectedPub.senderDappFr
                    || pub.senderAccFr != _expectedPub.senderAccFr
                    || pub.dappFr != _expectedPub.dappFr || pub.accFr != _expectedPub.accFr
                    || pub.nullifier != _expectedPub.nullifier
            ) {
                return false;
            }
        }

        if (useStrictLayerHashes) {
            for (uint256 i = 0; i < 100; i++) {
                if (layerHashes[i] != _expectedLayerHashes[i]) return false;
            }
        }

        return true;
    }
}
