// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IFallbackVerifier.sol";

/// @title MockFallbackVerifier
/// @notice Configurable mock for `IFallbackVerifier`. Used by Phase 4
///         `AckiNackiBridge.verifyBlock` tests to exercise the Fallback
///         finalization branch without generating a real Circuit 1B proof.
///         The real Fallback verifier itself is exercised end-to-end in
///         `AckiNackiBridgeProductionVerifyBlock.t.sol` against the production
///         `FallbackAggregatorVerifier.sol` (SHPLONK, K=21 inner).
///
/// @dev `IFallbackVerifier.verifyFallbackAttestation` is declared `view`;
///      this mock keeps that mutability and reads `shouldAccept` from
///      storage. State changes happen only through the explicit setter,
///      called from test setUp.
contract MockFallbackVerifier is IFallbackVerifier {
    /// @notice Toggle: when `true`, every call returns `true`; otherwise `false`.
    bool public shouldAccept;

    /// @dev BN254 scalar field order — see the note on `MockPrimaryVerifier`.
    ///      A public input `>= R` cannot equal any instance the real adapter
    ///      reads out of a proof, so accepting one here would let the suite
    ///      pass on an encoding production rejects.
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    function setShouldAccept(bool v) external {
        shouldAccept = v;
    }

    function verifyFallbackAttestation(
        bytes calldata, /* proof */
        uint256 blockId,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view returns (bool) {
        if (!shouldAccept) return false;
        return blockId < R && bkSetCommitment < R && blockSeqNo < R && lastSeenBlockSeqNo < R;
    }
}
