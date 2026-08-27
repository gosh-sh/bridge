// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title Bn254FrLib
/// @notice Test helper: reduce an arbitrary 256-bit word into a canonical
///         BN254 Fr so mock `verifyBlock` inputs survive the ETH-1/ETH-2
///         `FieldElementOutOfRange` gate (`value < r`). Production proofs
///         already expose canonical Fr; tests historically used raw keccak.
library Bn254FrLib {
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    /// @dev `x mod r`, except 0 maps to 1 so active layer slots stay non-zero.
    function toFr(uint256 x) internal pure returns (uint256 y) {
        y = x % R;
        if (y == 0) return 1;
    }
}
