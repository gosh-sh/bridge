// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IShplonkHalo2Verifier.sol";

/// @title ShplonkHalo2Verifier
/// @notice Thin wrapper around deployable Yul bytecode (CREATE from `.bin`).
contract ShplonkHalo2Verifier is IShplonkHalo2Verifier {
    /// @inheritdoc IShplonkHalo2Verifier
    function verify(bytes calldata instancesAndProof) external view returns (bool ok) {
        (ok,) = address(this).staticcall(instancesAndProof);
        return ok;
    }
}
