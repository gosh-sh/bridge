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

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyPrimaryAttestation(
        bytes calldata, /* proof */
        uint256, /* blockId */
        uint256, /* bkSetCommitment */
        uint256, /* blockSeqNo */
        uint256 /* lastSeenBlockSeqNo */
    )
        external
        view
        returns (bool)
    {
        return shouldAccept;
    }
}
