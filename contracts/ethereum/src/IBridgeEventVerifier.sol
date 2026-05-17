// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeEventVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Bridge Event**
///         proofs (Circuit 4 — `bridge-event-prove-circuit`). Each accepted
///         proof witnesses that the AN-side `TokenBridge` contract (identified
///         by `(dappFr, accFr)`) emitted a `WithdrawalInitiated` event for
///         `tokenId` whose hash chain anchors into one of the supplied
///         `layerHashes` (privately selected by the circuit).
///
/// @dev IMPORTANT — Phase A scope:
///      The Circuit 4 ABI as published by the partner today keeps
///      `dstChainId`, `amount`, `recipient` and `sender` as **private**
///      witnesses (see `EVENT_LAYOUT_COMPARISON.md` in
///      `bridge-event-prove-circuit/src/`). This means the bridge contract
///      learns *that* a withdrawal event was attested, but NOT *how much* to
///      pay or *to whom*. A genuine `withdraw()` is therefore NOT exposed on
///      `AckiNackiBridge.sol` in Phase A; the only consumer is
///      `verifyEvent(...)` which surfaces the attestation as an event for
///      off-chain consumption / future composition. See
///      `docs/circuit_4_open_questions.md` (Q-CIRC4-{1..5}) for the design
///      questions blocking a real `withdraw()` (chiefly: making
///      `amount`/`recipient` public + defining a replay-resistant nullifier).
interface IBridgeEventVerifier {
    /// @notice Verify a Circuit 4 (Bridge Event Prove) proof.
    /// @param proof 256-byte gnark Groth16 proof (8 × uint256, marshal-solidity layout).
    /// @param tokenId 32-bit ABI-encoded token id extracted from the event body.
    /// @param dappFr Fr-encoded AN-side bridge dApp identifier
    ///        (immutable on the consuming contract — guards against accepting
    ///         events emitted by *other* AN-side contracts).
    /// @param accFr  Fr-encoded AN-side bridge account identifier (immutable).
    /// @param layerHashes 100 candidate layer-hash field elements; the proof
    ///        binds to exactly one of them via a private `hash_choice_index`.
    ///        Pass the rolling `layerWindow` snapshot from
    ///        `AckiNackiBridge.getLayerWindow()`.
    /// @return isValid true on a passing proof; reverts in the underlying gnark
    ///         verifier are caught and surfaced as `false`.
    function verifyBridgeEvent(
        bytes calldata proof,
        uint256 tokenId,
        uint256 dappFr,
        uint256 accFr,
        uint256[100] calldata layerHashes
    ) external view returns (bool isValid);
}
