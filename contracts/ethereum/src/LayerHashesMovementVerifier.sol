// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./ILayerHashesMovementVerifier.sol";
import "./ILayerHashesGroth16Verifier.sol";

/// @title LayerHashesMovementVerifier
/// @notice Adapter that verifies Circuit 2 (Layer Hashes Movement) proofs from
///         Acki Nacki using the gnark-generated Groth16 verifier.
/// @dev The Halo2 SHPLONK proof from `bridge-prover-orchestrator` is wrapped
///      off-chain by `gnark-wrappers/circuit-2` into a 256-byte Groth16 proof.
///      This adapter re-assembles the 14 public inputs in the order the gnark
///      circuit expects (matches `LayerHashesProofOutput::instances` from
///      `bridge_prover_orchestrator::layer_hashes_prover`) and forwards to the
///      generated `verifyProof`.
///
///      Mirrors `PrimaryVerifier.sol` structure (the other retained gnark
///      Groth16 test-coverage adapter) but uses a 14-element input array
///      instead of 4.
contract LayerHashesMovementVerifier is ILayerHashesMovementVerifier {
    ILayerHashesGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;
    uint256 private constant NUM_LAYER_HASH_SLOTS = 10;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = ILayerHashesGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc ILayerHashesMovementVerifier
    function verifyLayerHashesMovement(
        bytes calldata proof,
        uint256 blockId,
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

        uint256[14] memory circuitInputs;
        circuitInputs[0] = blockId;
        circuitInputs[1] = bkSetCommitment;
        circuitInputs[2] = numLayers;
        for (uint256 i = 0; i < NUM_LAYER_HASH_SLOTS; i++) {
            circuitInputs[3 + i] = layerHashes[i];
        }
        circuitInputs[13] = prevMaxLevelLayerHash;

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
