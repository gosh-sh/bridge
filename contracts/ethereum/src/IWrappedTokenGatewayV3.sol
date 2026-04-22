// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IWrappedTokenGatewayV3
/// @notice Helper contract that wraps/unwraps native ETH around AAVE V3 Pool calls
/// @dev Mainnet deployment (AAVE V3 Ethereum): 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C.
///      Other networks differ — always pass the address in via constructor.
interface IWrappedTokenGatewayV3 {
    /// @notice Wrap `msg.value` to WETH and supply it to the AAVE pool.
    /// @param pool         AAVE V3 Pool address.
    /// @param onBehalfOf   Recipient of the minted aWETH.
    /// @param referralCode Optional referral code.
    function depositETH(address pool, address onBehalfOf, uint16 referralCode) external payable;

    /// @notice Withdraw WETH from the pool and unwrap to native ETH.
    /// @param pool    AAVE V3 Pool address.
    /// @param amount  Amount of WETH to withdraw, or type(uint256).max for full balance.
    /// @param to      Recipient of the unwrapped ETH.
    /// @dev Requires aWETH allowance from caller to this gateway.
    function withdrawETH(address pool, uint256 amount, address to) external;
}
