// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IPredecessorBridge
/// @notice The part of an earlier `AckiNackiBridge` deployment that its
///         successor on the same chain reads. Every `AckiNackiBridge` ever
///         deployed exposes these three views, so any of them can be named
///         as a predecessor.
interface IPredecessorBridge {
    /// @notice Whether the bridge has paid the withdrawal with this nullifier.
    ///         A bridge from this release on also answers for its own
    ///         `predecessor` and `legacyPredecessors`; an older one answers
    ///         only for itself.
    function isNullifierUsed(uint256 nullifier) external view returns (bool);

    function bridgeWithdrawalDappFr() external view returns (uint256);

    function bridgeWithdrawalAccFr() external view returns (uint256);
}
