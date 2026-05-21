// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeWithdrawalVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Bridge Withdrawal**
///         proofs (Circuit 4 v2 — Bridge Withdrawal Prove, Phase B). A passing
///         proof witnesses that the AN-side `TokenBridge` identified by
///         `(dappFr, accFr)` emitted a `WithdrawalInitiated` event with the
///         specified `amount` payable to `recipient` (an EVM address), submitted
///         by the AN-side actor identified by `(senderDappFr, senderAccFr)`,
///         targeting `dstChainId` (must equal Ethereum's `block.chainid`), and
///         binding to a unique `nullifier` for replay protection. The event is
///         anchored to one of the supplied `layerHashes` (privately selected by
///         the circuit's `hash_choice_index`).
///
/// @dev Phase B introduces the **withdrawal payout path** that Phase A
///      (`IBridgeEventVerifier`, attestation-only) cannot serve because Phase A's
///      circuit keeps `amount`/`recipient`/`dstChainId`/`sender` as private
///      witnesses. Both interfaces are wired in parallel during migration;
///      Phase A retires once the partner ships the v2 (110-input) circuit.
///
///      The `WithdrawalPublicInputs` struct mirrors slots [0..9] of the gnark
///      circuit's 110-element public-input vector; slots [10..109] are supplied
///      separately as `layerHashes` because they're sourced from the bridge's
///      own on-chain ring buffer (a caller can't substitute a known-good window
///      from a different bridge instance — the adapter pulls them on-chain).
interface IBridgeWithdrawalVerifier {
    /// @notice Public-input slots [0..9] of the Circuit 4 v2 proof.
    /// @dev Field order matches the gnark circuit's public-input layout
    ///      byte-for-byte. See `IBridgeWithdrawalGroth16Verifier` for the full
    ///      110-element vector and the slot semantics.
    struct WithdrawalPublicInputs {
        uint256 tokenId;
        uint256 amount;
        /// @notice Top half of the 20-byte EVM `recipient` address, packed as
        ///         Fr. Split convention pending Alina ack (Q-C4-1 follow-up:
        ///         α = 10/10 byte halves, β = 16/4, γ = 12/8). Default in our
        ///         scaffolding is α — `recipient[0..10]` → `recipientHi`,
        ///         `recipient[10..20]` → `recipientLo`.
        uint256 recipientHi;
        uint256 recipientLo;
        uint256 dstChainId;
        uint256 senderDappFr;
        uint256 senderAccFr;
        uint256 dappFr;
        uint256 accFr;
        uint256 nullifier;
    }

    /// @notice Verify a Circuit 4 v2 (Bridge Withdrawal Prove) proof.
    /// @param proof 256-byte gnark Groth16 proof (8 × uint256, marshal-solidity layout).
    /// @param pub Public-input slots [0..9] — the 10 event-derived field elements.
    /// @param layerHashes 100 candidate layer-hash field elements (slots [10..109]);
    ///        the proof binds (privately) to exactly one of them via the circuit's
    ///        `hash_choice_index`. Pass the snapshot from `AckiNackiBridge.getLayerWindow()`.
    /// @return isValid true on a passing proof; reverts inside the underlying gnark
    ///         verifier are caught and surfaced as `false`.
    function verifyWithdrawal(
        bytes calldata proof,
        WithdrawalPublicInputs calldata pub,
        uint256[100] calldata layerHashes
    ) external view returns (bool isValid);
}
