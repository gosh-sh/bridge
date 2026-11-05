// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title ArbitrumSepoliaAave
/// @notice The AAVE V3 USDC market every bridge on Arbitrum Sepolia (421614)
///         is deployed against. Its underlying is Circle's USDC. Read
///         2026-10-28: the aToken's `UNDERLYING_ASSET_ADDRESS` is `USDC` and
///         its `POOL` is `POOL`.
library ArbitrumSepoliaAave {
    uint256 internal constant CHAIN_ID = 421_614;
    address internal constant USDC = 0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d;
    address internal constant POOL = 0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff;
    address internal constant AUSDC = 0x460b97BD498E1157530AEb3086301d5225b91216;
}
