// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IDepositBridge
/// @notice The slice of `AckiNackiBridge` a hub receiver needs.
///         One deployment custodies one token (`usdc()`). A second token is a
///         second bridge and a second receiver, not a second argument here.
///         `eccUSDCBridge.confirmDeposit` always mints USDC ECC, so a
///         non-USDC hub bridge must not be passed to `setTrustedL1Bridge`.
interface IDepositBridge {
    function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) external;

    function usdc() external view returns (address);
}
