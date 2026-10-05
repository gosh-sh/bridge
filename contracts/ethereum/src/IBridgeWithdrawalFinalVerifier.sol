// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeWithdrawalFinalVerifier
/// @notice Bridge-side interface for the **multi-thread** successor of
///         `IBridgeWithdrawalVerifier`. Verifies `BridgeEventFinalProof`
///         (Rust: `bridge_event_prove_circuit::bridge_event_final_proof`).
///
///         The public-input vector grows from 11 to 13 slots. Slots `[0..=10]`
///         mirror `IBridgeWithdrawalVerifier.WithdrawalPublicInputs`
///         byte-for-byte; slots `[11..=12]` are the new endpoints that
///         `withdrawByProofBundle` binds to a hop-chain of
///         `IBridgeMultiHopVerifier` proofs (same-thread claims: `xBlockId ==
///         yBlockId` and the hop chain is empty).
///
///         The two verifier interfaces are wired independently on
///         `AckiNackiBridge`; the on-chain bundle acceptance gate mirrors
///         `bridge-event-prove-circuit::bundle_verifier::verify_bundle`.
interface IBridgeWithdrawalFinalVerifier {
    /// @notice 13 public inputs of `BridgeEventFinalProof`. Slots `[0..=10]`
    ///         are byte-identical to
    ///         `IBridgeWithdrawalVerifier.WithdrawalPublicInputs`.
    struct WithdrawalFinalPublicInputs {
        uint256 tokenId;
        uint256 amount;
        /// @notice Top 10 bytes of the 20-byte EVM `recipient` address.
        uint256 recipientHi;
        /// @notice Bottom 10 bytes.
        uint256 recipientLo;
        uint256 dstChainId;
        uint256 senderAccFr;
        uint256 dappFr;
        uint256 accFr;
        /// @notice Replay-protection nullifier.
        uint256 nullifier;
        /// @notice Dense-chain anchor the proof binds to.
        uint256 finalRoot;
        /// @notice 1-indexed layer window (`1..=MAX_LAYER_HASHES`).
        uint256 anchorLayer;
        /// @notice Fr-encoding of the X-block's `block_id` (event block).
        ///         Same-thread claims: `xBlockId == yBlockId`.
        uint256 xBlockId;
        /// @notice Fr-encoding of the Y-block's `block_id` (anchor block).
        ///         For cross-thread claims, the hop chain walks
        ///         `yBlockId → xBlockId`.
        uint256 yBlockId;
    }

    /// @notice Verify a `BridgeEventFinalProof` (13-instance) proof.
    /// @param proof SHPLONK proof bytes (aggregator calldata: instances ‖ proof).
    /// @param pub Public-input slots — the 13 field elements.
    /// @return isValid true on a passing proof; reverts inside the underlying
    ///         SHPLONK verifier are caught and surfaced as `false`.
    function verifyWithdrawalFinal(
        bytes calldata proof,
        WithdrawalFinalPublicInputs calldata pub
    ) external view returns (bool isValid);
}
