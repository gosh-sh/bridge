// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IPrimaryVerifier.sol";

/// @title MockPrimaryVerifier
/// @notice Configurable mock for `IPrimaryVerifier`. Used by Phase 5
///         relayer-loop tests to drive `AckiNackiBridge.verifyBlock` through
///         many sequential blocks without spawning the real
///         Halo2 → gnark prove pipeline for each one. The production
///         `PrimaryVerifier` itself is independently covered by the bound
///         end-to-end test in `AckiNackiBridgeVerifyBlock.t.sol`.
///
/// @dev `IPrimaryVerifier.verifyPrimaryAttestation` is declared `view`, so the
///      mock keeps that mutability and reads `shouldAccept` from storage. The
///      flag is flipped via the explicit setter from test setUp.
contract MockPrimaryVerifier is IPrimaryVerifier {
    /// @notice Toggle: when `true`, every call returns `true`; otherwise `false`.
    bool public shouldAccept;

    /// @dev BN254 scalar field order. `shouldAccept` cannot wave through a
    ///      non-canonical argument: the real adapter compares each one
    ///      byte-for-byte against an instance read out of the proof
    ///      (`PrimaryAggregatorVerifier._readInstance`), and instances are
    ///      always `< R`, so anything `>= R` returns false there no matter how
    ///      valid the proof is. A mock that ignored this would keep passing on
    ///      encodings production rejects — which is exactly how the raw-vs-`Fr`
    ///      `blockId` mismatch in `applyBkSetUpdate` stayed invisible.
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyPrimaryAttestation(
        bytes calldata, /* proof */
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    )
        external
        view
        returns (bool)
    {
        if (!shouldAccept) return false;
        return blockId < R && bkSetCommitment < R && blockSeqNo < R && lastSeenBlockSeqNo < R;
    }
}
