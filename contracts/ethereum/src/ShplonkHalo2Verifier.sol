// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IShplonkHalo2Verifier.sol";

/// @title ShplonkHalo2Verifier
/// @notice Forwards `verify()` to snark-verifier-sdk Yul bytecode deployed via CREATE
///         from a `.bin` artefact (fallback entrypoint on the Yul contract).
contract ShplonkHalo2Verifier is IShplonkHalo2Verifier {
    address public immutable yulVerifier;

    error InvalidYulVerifierAddress();
    /// @notice Yul verifier must have deployed bytecode (QC-A4-1).
    error EmptyYulVerifierCode();

    /// @notice Gas stipend for the Yul `staticcall`. Measured accepts on the
    ///         committed artefacts (`ShplonkArtefactPairing.t.sol`): Primary
    ///         425_012, Fallback 425_012, LayerHashes 431_024, Withdrawal
    ///         398_082. 1.5 M is at least 3.5x margin on every accept. A
    ///         reject lands at 1_503_751, at the cap. All four accepts are
    ///         asserted `< VERIFY_GAS_CAP` there; regen must re-run those tests.
    uint256 public constant VERIFY_GAS_CAP = 1_500_000;

    /// @param _yulVerifier Address returned by CREATE-deploying the exported `.bin` bytecode.
    constructor(address _yulVerifier) {
        if (_yulVerifier == address(0)) revert InvalidYulVerifierAddress();
        uint256 codeSize;
        assembly {
            codeSize := extcodesize(_yulVerifier)
        }
        if (codeSize == 0) revert EmptyYulVerifierCode();
        yulVerifier = _yulVerifier;
    }

    /// @inheritdoc IShplonkHalo2Verifier
    function verify(bytes calldata instancesAndProof) external view returns (bool ok) {
        (ok,) = yulVerifier.staticcall{ gas: VERIFY_GAS_CAP }(instancesAndProof);
    }
}
