// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./ILayerHashVerifier.sol";
import "./LayerHashGroth16Verifier.sol";

/// @title LayerHashVerifier
/// @notice Adapter that verifies layer hash update proofs from Acki Nacki using
///         a gnark-generated Groth16 verifier.
/// @dev Public input layout (13 BN254 Fr elements, matching the Halo2 circuit):
///      [0]     bkSetCommitment
///      [1]     numLayers
///      [2..11] layerHashes[0..10]
///      [12]    prevMaxLevelLayerHash
contract LayerHashVerifier is ILayerHashVerifier {
    ILayerHashGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = ILayerHashGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc ILayerHashVerifier
    function verifyLayerHashUpdate(
        bytes calldata proof,
        uint256 bkSetCommitment,
        uint256 numLayers,
        uint256[10] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external view override returns (bool isValid) {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        uint256[13] memory circuitInputs;
        circuitInputs[0] = bkSetCommitment;
        circuitInputs[1] = numLayers;
        for (uint256 i = 0; i < 10; i++) {
            circuitInputs[2 + i] = layerHashes[i];
        }
        circuitInputs[12] = prevMaxLevelLayerHash;

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
