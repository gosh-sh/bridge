// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IShplonkHalo2Verifier.sol";

/// @title ShplonkAggregatorVerifierBase
/// @notice Shared helpers for R15 aggregator adapters: 12 KZG accumulator limbs
///         followed by re-exposed inner public inputs in calldata.
abstract contract ShplonkAggregatorVerifierBase {
    /// @notice KZG pairing accumulator limb count (snark-verifier-sdk layout).
    uint256 internal constant NUM_ACCUMULATOR_INSTANCES = 12;

    IShplonkHalo2Verifier public immutable shplonkVerifier;

    error InvalidVerifierAddress();

    constructor(address _shplonkVerifier) {
        if (_shplonkVerifier == address(0)) revert InvalidVerifierAddress();
        shplonkVerifier = IShplonkHalo2Verifier(_shplonkVerifier);
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
