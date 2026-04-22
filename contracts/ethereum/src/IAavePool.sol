// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IAavePool
/// @notice Minimal interface for AAVE V3 Pool (only methods the bridge needs)
/// @dev Full ABI: https://github.com/aave/aave-v3-core
interface IAavePool {
    /// @notice Supply an asset to the pool and mint aTokens to `onBehalfOf`.
    /// @param asset       The underlying asset address (WETH for our use).
    /// @param amount      The amount to supply (asset units).
    /// @param onBehalfOf  Recipient of the minted aTokens.
    /// @param referralCode Optional referral code (0 if unused).
    function supply(address asset, uint256 amount, address onBehalfOf, uint16 referralCode) external;

    /// @notice Withdraw a previously supplied asset.
    /// @param asset   The underlying asset address.
    /// @param amount  The amount to withdraw, or type(uint256).max for full balance.
    /// @param to      Recipient of the withdrawn underlying asset.
    /// @return The final amount withdrawn.
    function withdraw(address asset, uint256 amount, address to) external returns (uint256);

    /// @notice Return the aToken (and other reserve) addresses for an asset.
    /// @dev Used only off-chain / in tests — the bridge learns aToken via its constructor.
    function getReserveData(address asset) external view returns (ReserveData memory);

    struct ReserveData {
        ReserveConfigurationMap configuration;
        uint128 liquidityIndex;
        uint128 currentLiquidityRate;
        uint128 variableBorrowIndex;
        uint128 currentVariableBorrowRate;
        uint128 currentStableBorrowRate;
        uint40 lastUpdateTimestamp;
        uint16 id;
        address aTokenAddress;
        address stableDebtTokenAddress;
        address variableDebtTokenAddress;
        address interestRateStrategyAddress;
        uint128 accruedToTreasury;
        uint128 unbacked;
        uint128 isolationModeTotalDebt;
    }

    struct ReserveConfigurationMap {
        uint256 data;
    }
}
