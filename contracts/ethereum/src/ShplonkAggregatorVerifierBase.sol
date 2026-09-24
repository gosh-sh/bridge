// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IShplonkHalo2Verifier.sol";

/// @title ShplonkAggregatorVerifierBase
/// @notice Shared helpers for R15 aggregator adapters: 12 KZG accumulator limbs,
///         followed by re-exposed inner public inputs, and a Poseidon digest of
///         the inner-circuit VK witnesses at the tail. The digest is pinned at
///         deploy time via `_vkDigest`; every adapter rejects proofs whose
///         exposed digest doesn't match — otherwise the aggregator would accept
///         a proof from any inner circuit with the same shape (# columns,
///         # instances, gate/lookup arity) even if its constraints differ.
abstract contract ShplonkAggregatorVerifierBase {
    /// @notice KZG pairing accumulator limb count (snark-verifier-sdk layout).
    uint256 internal constant NUM_ACCUMULATOR_INSTANCES = 12;

    /// @notice BN254 scalar-field modulus `r`. The aggregator's Poseidon
    ///         digest is always a valid Fr, so any pin at or above `r`
    ///         cannot match the on-chain slot and would silently reject
    ///         every proof.
    uint256 internal constant BN254_FR_MODULUS = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    IShplonkHalo2Verifier public immutable shplonkVerifier;

    /// @notice Poseidon digest of the inner-circuit VK witnesses. Matches the
    ///         value emitted at instance-column position `12 + NUM_INNER`.
    bytes32 public immutable vkDigest;

    error InvalidVerifierAddress();
    /// @notice Reject `bytes32(0)` at deploy time. A Poseidon output over
    ///         BN254's scalar field is not zero in practice, so a zero pin
    ///         would in fact reject every honest proof — the guard catches
    ///         an unset constructor argument, not a matching zero digest.
    error InvalidVkDigest();
    /// @notice Reject a pin `>= r` at deploy time. A valid Fr digest is
    ///         strictly less than `r`, so a pin above `r` cannot match any
    ///         real aggregator output and would silently reject every proof.
    ///         Catches an operator who passed a raw 32-byte hash or a
    ///         chain-id-derived value in place of a real Fr digest.
    error VkDigestExceedsFieldModulus();

    constructor(address _shplonkVerifier, bytes32 _vkDigest) {
        if (_shplonkVerifier == address(0)) revert InvalidVerifierAddress();
        if (_vkDigest == bytes32(0)) revert InvalidVkDigest();
        if (uint256(_vkDigest) >= BN254_FR_MODULUS) revert VkDigestExceedsFieldModulus();
        shplonkVerifier = IShplonkHalo2Verifier(_shplonkVerifier);
        vkDigest = _vkDigest;
    }

    /// @dev Read one 32-byte little-endian Fr instance from `data` at `index`.
    function _readInstance(bytes calldata data, uint256 index) internal pure returns (uint256 v) {
        uint256 offset = index * 32;
        require(data.length >= offset + 32, "short instances");
        v = uint256(bytes32(data[offset:offset + 32]));
    }

    function _verifyShplonk(bytes calldata instancesAndProof) internal view returns (bool) {
        return shplonkVerifier.verify(instancesAndProof);
    }
}
