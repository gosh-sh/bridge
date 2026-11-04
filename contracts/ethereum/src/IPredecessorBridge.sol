// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IPredecessorBridge
/// @notice The part of an earlier `AckiNackiBridge` deployment that its
///         successor on the same chain reads before taking over its
///         treasury. Every `AckiNackiBridge` ever deployed exposes these
///         views, so any of them can be named as a predecessor.
interface IPredecessorBridge {
    function bridgeWithdrawalDappFr() external view returns (uint256);

    function bridgeWithdrawalAccFr() external view returns (uint256);
}
