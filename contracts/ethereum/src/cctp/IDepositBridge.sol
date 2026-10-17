// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IDepositBridge
/// @notice The slice of `AckiNackiBridge` a hub receiver needs.
///         One deployment custodies one token (`usdc()`). A second token is a
///         second bridge and a second receiver, not a second argument here —
///         the deposit proof binds the emitting contract, and the Acki Nacki
///         side maps that contract to an ECC id.
interface IDepositBridge {
    function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) external;

    function usdc() external view returns (address);
}
