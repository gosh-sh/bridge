// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeWithdrawalVerifier.sol";
import "./IBridgeWithdrawalGroth16Verifier.sol";

/// @title BridgeWithdrawalVerifier
/// @notice **FORBIDDEN for production / mainnet deploys (WD-Q3).**
///         Legacy Groth16 adapter for Circuit 4. The gnark wrapper is an
///         **identity stub** — it does **not** verify the Halo2 SHPLONK proof.
///         Production must wire `BridgeWithdrawalAggregatorVerifier` via
///         `ShplonkDeployLib.deployWithdrawalAdapter` only.
///
/// @dev Kept for historical / test reference. CI gate:
///      `scripts/check_withdrawal_verifier_not_stub.sh`.
contract BridgeWithdrawalVerifier is IBridgeWithdrawalVerifier {
    IBridgeWithdrawalGroth16Verifier public immutable groth16Verifier;

    uint256 private constant GROTH16_PROOF_SIZE = 256;
    uint256 private constant NUM_PUBLIC_INPUTS = 10;

    error InvalidVerifierAddress();

    constructor(address _groth16Verifier) {
        if (_groth16Verifier == address(0)) {
            revert InvalidVerifierAddress();
        }
        groth16Verifier = IBridgeWithdrawalGroth16Verifier(_groth16Verifier);
    }

    /// @inheritdoc IBridgeWithdrawalVerifier
    function verifyWithdrawal(bytes calldata proof, WithdrawalPublicInputs calldata pub)
        external
        view
        override
        returns (bool isValid)
    {
        if (proof.length != GROTH16_PROOF_SIZE) {
            return false;
        }

        uint256[8] memory groth16Proof;
        for (uint256 i = 0; i < 8; i++) {
            groth16Proof[i] = uint256(bytes32(proof[i * 32:(i + 1) * 32]));
        }

        // Slot order matches `bridge_event_prove_circuit::PUB_*` (partner
        // branch `circuit4-single-final-root`):
        //   [0] tokenId      [5] senderAccFr
        //   [1] amount       [6] dappFr
        //   [2] recipientHi  [7] accFr
        //   [3] recipientLo  [8] nullifier
        //   [4] dstChainId   [9] finalRoot
        uint256[NUM_PUBLIC_INPUTS] memory circuitInputs;
        circuitInputs[0] = pub.tokenId;
        circuitInputs[1] = pub.amount;
        circuitInputs[2] = pub.recipientHi;
        circuitInputs[3] = pub.recipientLo;
        circuitInputs[4] = pub.dstChainId;
        circuitInputs[5] = pub.senderAccFr;
        circuitInputs[6] = pub.dappFr;
        circuitInputs[7] = pub.accFr;
        circuitInputs[8] = pub.nullifier;
        circuitInputs[9] = pub.finalRoot;

        try groth16Verifier.verifyProof(groth16Proof, circuitInputs) {
            return true;
        } catch {
            return false;
        }
    }
}
