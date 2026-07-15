// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @notice Minimal interface for snark-verifier-sdk Yul output (`Halo2Verifier`).
interface IShplonkHalo2Verifier {
    function verify(bytes calldata instancesAndProof) external view returns (bool ok);
}
