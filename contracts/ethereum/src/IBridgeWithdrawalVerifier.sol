// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IBridgeWithdrawalVerifier
/// @notice Bridge-side interface for verifying Acki Nacki **Bridge Withdrawal**
///         proofs (Circuit 4 — `bridge-event-prove-circuit`). A passing
///         proof witnesses that the AN-side `eccUSDCBridge` identified by
///         `(dappFr, accFr)` emitted a `WithdrawalInitiated` event with the
///         specified `amount` payable to `recipient` (an EVM address), submitted
///         by the AN-side actor identified by `senderAccFr`, targeting
///         `dstChainId` (must equal Ethereum's `block.chainid`), and binding
///         to a unique `nullifier` for replay protection. The event is
///         anchored off-circuit to one specific `finalRoot` — the verifier
///         (this adapter, on-chain) checks that `finalRoot` is in the
///         bridge's set of known anchors (populated by `verifyBlock`).
///
///         The AN-side `eccUSDCBridge` emits:
///             event WithdrawalInitiated(
///                 uint256 dstChainId,
///                 bytes   recipient,
///                 uint128 amount,
///                 uint32  tokenId,
///                 address sender
///             );
///
///         The `WithdrawalPublicInputs` struct below mirrors slots [0..9] of
///         the Halo2 circuit's 10-element public-input vector byte-for-byte.
interface IBridgeWithdrawalVerifier {
    /// @notice Public-input slots [0..9] of the Circuit 4 proof.
    /// @dev Field order matches the Halo2 circuit's public-input layout
    ///      byte-for-byte. The on-chain adapter forwards this struct
    ///      verbatim to `verifyProof`.
    struct WithdrawalPublicInputs {
        uint256 tokenId;
        uint256 amount;
        /// @notice Top 10 bytes of the 20-byte EVM `recipient` address, BE-packed
        ///         as Fr. Split convention α (10/10) per partner circuit doc.
        uint256 recipientHi;
        /// @notice Bottom 10 bytes (split α, see `recipientHi`).
        uint256 recipientLo;
        uint256 dstChainId;
        /// @notice Fr-encoding of the AN-side sender's 256-bit `account_id`
        ///         (decoded algebraically from the sender cell's
        ///         `cell_repr_data` bits [11..267)).
        uint256 senderAccFr;
        /// @notice Bridge dApp identifier on AN side (immutable per deployment).
        uint256 dappFr;
        /// @notice Bridge account identifier on AN side (immutable per deployment).
        uint256 accFr;
        /// @notice Replay-protection nullifier — `Poseidon(block_id_fr,
        ///         tokenId, amount, recipientHi, recipientLo, senderAccFr)`.
        uint256 nullifier;
        /// @notice The dense-chain anchor the proof binds to. The bridge
        ///         contract independently checks `finalRoot` against its
        ///         set of known anchors (populated on every successful
        ///         `verifyBlock`); the circuit only proves that *some*
        ///         chain extension lands at this value.
        uint256 finalRoot;
    }

    /// @notice Verify a Circuit 4 (single-final-root) proof.
    /// @param proof SHPLONK proof bytes (Halo2 KZG aggregator calldata: instances ‖ proof).
    /// @param pub Public-input slots [0..9] — the 10 field elements.
    /// @return isValid true on a passing proof; reverts inside the underlying SHPLONK
    ///         verifier are caught and surfaced as `false`.
    function verifyWithdrawal(bytes calldata proof, WithdrawalPublicInputs calldata pub)
        external
        view
        returns (bool isValid);
}
