// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IPrimaryVerifier.sol";
import "./IPrimaryGroth16Verifier.sol";

/// @title PrimaryVerifier
/// @notice Adapter that verifies Circuit 1A (Primary attestation) proofs from
///         Acki Nacki using the gnark-generated Groth16 verifier.
/// @dev The Halo2 SHPLONK proof from `bridge-prover-orchestrator` is wrapped
///      off-chain by `gnark-wrappers/circuit-1a` into a 256-byte Groth16
///      proof. This adapter re-assembles the 4 public inputs in the order the
///      gnark circuit expects (matches `bridge_prover_lib::ProofOutput`'s
///      instance ordering) and forwards to the generated `verifyProof`.
///      Mirrors `FallbackVerifier.sol` exactly — only the verifier address is
///      different.
contract PrimaryVerifier is IPrimaryVerifier {
    IPrimaryGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = IPrimaryGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc IPrimaryVerifier
    function verifyPrimaryAttestation(
        bytes calldata proof,
        uint256 envelopeHash,
        uint256 bkSetCommitment,
        uint256 blockSeqNo,
        uint256 lastSeenBlockSeqNo
    ) external view override returns (bool isValid) {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        uint256[4] memory circuitInputs;
        circuitInputs[0] = envelopeHash;
        circuitInputs[1] = bkSetCommitment;
        circuitInputs[2] = blockSeqNo;
        circuitInputs[3] = lastSeenBlockSeqNo;

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
