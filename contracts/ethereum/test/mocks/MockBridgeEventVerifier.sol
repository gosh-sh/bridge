// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IBridgeEventVerifier.sol";

/// @title MockBridgeEventVerifier
/// @notice Configurable mock for `IBridgeEventVerifier`. Used by the Phase A
///         Circuit 4 tests to exercise the `verifyEvent` entrypoint and the
///         `layerWindow` plumbing without spinning up the (yet to be built)
///         halo2 → gnark pipeline for Circuit 4.
///
/// @dev Two modes:
///      - **Loose** (default): returns `shouldAccept` regardless of the
///        forwarded args.
///      - **Strict**: if `useStrictLayerHashes == true`, the mock also
///        requires the forwarded `layerHashes` array to equal
///        `_expectedLayerHashes` byte-for-byte. This is how tests prove that
///        the bridge actually forwarded its own on-chain window snapshot —
///        the mock can't record from a `view` function, but it *can* refuse
///        anything other than the known-correct snapshot, so a passing test
///        is itself the proof.
///      - **Token/identity pin** (optional): when `useStrictIdentity == true`,
///        the mock also requires (`tokenId`, `dappFr`, `accFr`) to match
///        the configured triple — used to assert that `verifyEvent` forwards
///        the immutable `bridgeEventDappFr` / `bridgeEventAccFr` correctly.
contract MockBridgeEventVerifier is IBridgeEventVerifier {
    bool public shouldAccept;

    bool public useStrictLayerHashes;
    uint256[100] private _expectedLayerHashes;

    bool public useStrictIdentity;
    uint256 public expectedTokenId;
    uint256 public expectedDappFr;
    uint256 public expectedAccFr;

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

    function setExpectedIdentity(uint256 tokenId, uint256 dappFr, uint256 accFr) external {
        expectedTokenId = tokenId;
        expectedDappFr = dappFr;
        expectedAccFr = accFr;
        useStrictIdentity = true;
    }

    function clearStrictIdentity() external {
        useStrictIdentity = false;
    }

    function verifyBridgeEvent(
        bytes calldata, /* proof */
        uint256 tokenId,
        uint256 dappFr,
        uint256 accFr,
        uint256[100] calldata layerHashes
    ) external view override returns (bool) {
        if (!shouldAccept) return false;

        if (useStrictIdentity) {
            if (tokenId != expectedTokenId || dappFr != expectedDappFr || accFr != expectedAccFr) {
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
