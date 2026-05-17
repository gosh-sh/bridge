// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeEventVerifier.sol";
import "./IBridgeEventGroth16Verifier.sol";

/// @title BridgeEventVerifier
/// @notice Adapter that verifies Circuit 4 (Bridge Event Prove) proofs from
///         Acki Nacki using a gnark-generated Groth16 verifier.
///
/// @dev Mirrors `LayerHashesMovementVerifier.sol` structure: re-assembles the
///      103 public inputs in the order the gnark circuit expects and forwards
///      to the generated `verifyProof`. The generated verifier is wired at
///      construction time and immutable thereafter.
///
///      The Halo2 SHPLONK proof from `bridge-prover-orchestrator` (the
///      Circuit 4 path is *planned*; see `Phase B` items in
///      `docs/circuit_4_open_questions.md`) is wrapped off-chain by
///      `gnark-wrappers/circuit-4` into a 256-byte Groth16 proof.
contract BridgeEventVerifier is IBridgeEventVerifier {
    IBridgeEventGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;
    uint256 private constant NUM_LAYER_HASHES = 100;
    uint256 private constant NUM_PUBLIC_INPUTS = 3 + NUM_LAYER_HASHES;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = IBridgeEventGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc IBridgeEventVerifier
    function verifyBridgeEvent(
        bytes calldata proof,
        uint256 tokenId,
        uint256 dappFr,
        uint256 accFr,
        uint256[100] calldata layerHashes
    ) external view override returns (bool isValid) {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        uint256[NUM_PUBLIC_INPUTS] memory circuitInputs;
        circuitInputs[0] = tokenId;
        circuitInputs[1] = dappFr;
        circuitInputs[2] = accFr;
        for (uint256 i = 0; i < NUM_LAYER_HASHES; i++) {
            circuitInputs[3 + i] = layerHashes[i];
        }

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
