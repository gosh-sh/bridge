// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeWithdrawalVerifier.sol";
import "./IBridgeWithdrawalGroth16Verifier.sol";

/// @title BridgeWithdrawalVerifier
/// @notice Adapter that verifies Circuit 4 v2 (Bridge Withdrawal Prove) proofs
///         from Acki Nacki using a gnark-generated Groth16 verifier with 110
///         public inputs.
///
/// @dev Mirrors `BridgeEventVerifier.sol`'s shape but for the Phase B circuit
///      that exposes `amount`/`recipient`/`dstChainId`/`sender*`/`nullifier`
///      as public inputs. Assembles the 110-element vector in the exact
///      ordering documented in `IBridgeWithdrawalGroth16Verifier` and
///      forwards to the generated verifier. The generated verifier is wired
///      at construction time and immutable thereafter.
///
///      The Halo2 SHPLONK proof from the partner's `bridge-prover-orchestrator`
///      Circuit 4 path (Phase B; partner ack pending — see
///      `docs/an_partner_questions_circuit4_2026-05-17.md`) is wrapped
///      off-chain by `gnark-wrappers/circuit-4` into a 256-byte Groth16 proof.
contract BridgeWithdrawalVerifier is IBridgeWithdrawalVerifier {
    IBridgeWithdrawalGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;
    uint256 private constant NUM_LAYER_HASHES = 100;
    /// @notice Total public inputs: 10 event-derived slots + 100 layer hashes.
    uint256 private constant NUM_PUBLIC_INPUTS = 10 + NUM_LAYER_HASHES;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = IBridgeWithdrawalGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc IBridgeWithdrawalVerifier
    function verifyWithdrawal(
        bytes calldata proof,
        WithdrawalPublicInputs calldata pub,
        uint256[NUM_LAYER_HASHES] calldata layerHashes
    ) external view override returns (bool isValid) {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        uint256[NUM_PUBLIC_INPUTS] memory circuitInputs;
        circuitInputs[0] = pub.tokenId;
        circuitInputs[1] = pub.amount;
        circuitInputs[2] = pub.recipientHi;
        circuitInputs[3] = pub.recipientLo;
        circuitInputs[4] = pub.dstChainId;
        circuitInputs[5] = pub.senderDappFr;
        circuitInputs[6] = pub.senderAccFr;
        circuitInputs[7] = pub.dappFr;
        circuitInputs[8] = pub.accFr;
        circuitInputs[9] = pub.nullifier;
        for (uint256 i = 0; i < NUM_LAYER_HASHES; i++) {
            circuitInputs[10 + i] = layerHashes[i];
        }

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
