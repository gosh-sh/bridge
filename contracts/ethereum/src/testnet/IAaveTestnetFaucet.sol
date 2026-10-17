// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @notice The Aave V3 testnet faucet. Anyone may mint, up to a per-call cap
///         (10 000 tokens on Sepolia).
interface IAaveTestnetFaucet {
    function mint(address token, address to, uint256 amount) external returns (uint256);
}
