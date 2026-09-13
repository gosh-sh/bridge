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

    /// @notice Gas stipend for the Yul `staticcall` (ETH-19). Circuit 4 accept
    ///         is ~412k on the committed artefact; 1.5M is ~3.6× margin. A
    ///         reject otherwise burns ~97% of the remaining tx gas. Revisit
    ///         when 1A/1B/C2 pairing is regenerated (those accepts are unmeasured).
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
