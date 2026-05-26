// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBridgeWithdrawalVerifier.sol";
import "./IBridgeWithdrawalGroth16Verifier.sol";

/// @title BridgeWithdrawalVerifier
/// @notice Adapter that verifies Circuit 4 (`bridge-event-prove-circuit`,
///         single-final-root layout — partner branch
///         `circuit4-single-final-root`) proofs from Acki Nacki using a
///         gnark-generated Groth16 verifier with **10 public inputs**.
///
/// @dev Mirrors `LayerHashesMovementVerifier.sol` structure: re-assembles
///      the 10 public inputs in the order the gnark circuit expects and
///      forwards to the generated `verifyProof`. The generated verifier
///      is wired at construction time and immutable thereafter.
///
///      The Halo2 SHPLONK proof from `bridge-prover-orchestrator`
///      (Circuit 4 path, partner-shipped) is wrapped off-chain by
///      `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4` into
///      a 256-byte Groth16 proof.
///
///      **R15**: as of 2026-05-26 the gnark wrapper enforces only
///      identity-stub assertions over the public inputs — the Halo2
///      SHPLONK proof itself is **not** verified inside Groth16.
///      Replacing the wrapper with a real Halo2-in-gnark verifier is
///      tracked as Phase 8 of `docs/an_partner_integration_plan.md` and
///      is the single biggest open mainnet blocker.
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
