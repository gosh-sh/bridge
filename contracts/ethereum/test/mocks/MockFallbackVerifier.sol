// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IFallbackVerifier.sol";

/// @title MockFallbackVerifier
/// @notice Configurable mock for `IFallbackVerifier`. Used by Phase 4
///         `AckiNackiBridge.verifyBlock` tests to exercise the Fallback
///         finalization branch without spinning up another real Halo2 →
///         gnark wrap pipeline. The real Fallback verifier itself is
///         exercised in `FallbackVerifier.t.sol` against the production
///         `FallbackGroth16VerifierGenerated.sol`.
///
/// @dev `IFallbackVerifier.verifyFallbackAttestation` is declared `view`;
///      this mock keeps that mutability and reads `shouldAccept` from
///      storage. State changes happen only through the explicit setter,
///      called from test setUp.
contract MockFallbackVerifier is IFallbackVerifier {
    /// @notice Toggle: when `true`, every call returns `true`; otherwise `false`.
    bool public shouldAccept;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyFallbackAttestation(
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
