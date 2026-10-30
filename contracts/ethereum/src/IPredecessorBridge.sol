// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IPredecessorBridge
/// @notice The part of an earlier `AckiNackiBridge` deployment that its
///         successor on the same chain reads. Every `AckiNackiBridge` ever
///         deployed exposes these three views, so any of them can be named
///         as a predecessor.
interface IPredecessorBridge {
    /// @notice Whether the bridge, or any bridge it names as its own
    ///         predecessor, has paid the withdrawal with this nullifier.
    function isNullifierUsed(uint256 nullifier) external view returns (bool);

    function bridgeWithdrawalDappFr() external view returns (uint256);

    function bridgeWithdrawalAccFr() external view returns (uint256);
}
