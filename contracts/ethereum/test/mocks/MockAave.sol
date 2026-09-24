// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IAavePool.sol";
import "../../src/IERC20.sol";
import "./MockERC20.sol";

/// @title MockAUSDC
/// @notice Minimal 6-decimal aToken with test-only mint/burn hooks.
contract MockAUSDC is MockERC20 {
    constructor() MockERC20("Mock aUSDC", "maUSDC", 6) { }

    function mintTo(address to, uint256 amount) external {
        mint(to, amount);
    }

    function burnFrom(address from, uint256 amount) external {
        _burn(from, amount);
    }

    /// @notice Test-only: simulate accrued yield by minting extra aTokens.
    function accrueYield(address to, uint256 amount) external {
        mint(to, amount);
    }
}

/// @title MockAavePool
/// @notice Test-only AAVE V3 Pool that proxies ERC-20 `supply`/`withdraw` to MockAUSDC.
contract MockAavePool is IAavePool {
    IERC20 public immutable underlying;
    MockAUSDC public immutable aToken;
    /// @notice PoC: when `withdraw(max)` is called, leave this many aTokens
    ///         unburned (simulates a pool that under-redeems).
    uint256 public leftoverOnMaxWithdraw;
    /// @notice Mint this many fewer aTokens than USDC pulled on `supply`.
    uint256 public supplyHaircut;
    /// @notice Pay this many fewer USDC than aTokens burned on `withdraw`.
    uint256 public redeemHaircut;

    constructor(address _underlying, address _aToken) {
        underlying = IERC20(_underlying);
        aToken = MockAUSDC(_aToken);
    }

    function setLeftoverOnMaxWithdraw(uint256 leftover) external {
        leftoverOnMaxWithdraw = leftover;
    }

    function setSupplyHaircut(uint256 haircut) external {
        supplyHaircut = haircut;
    }

    function setRedeemHaircut(uint256 haircut) external {
        redeemHaircut = haircut;
    }

    function supply(address, uint256 amount, address onBehalfOf, uint16) external override {
        require(underlying.transferFrom(msg.sender, address(this), amount), "pool: pull");
        uint256 minted = amount > supplyHaircut ? amount - supplyHaircut : 0;
        if (minted > 0) aToken.mintTo(onBehalfOf, minted);
    }

    function withdraw(address, uint256 amount, address to) external override returns (uint256) {
        uint256 bal = aToken.balanceOf(msg.sender);
        uint256 toBurn = amount == type(uint256).max ? bal : amount;
        if (amount == type(uint256).max && leftoverOnMaxWithdraw > 0 && leftoverOnMaxWithdraw < bal)
        {
            toBurn = bal - leftoverOnMaxWithdraw;
        }
        require(bal >= toBurn, "pool: insufficient aUSDC");
        aToken.burnFrom(msg.sender, toBurn);
        uint256 payout = toBurn > redeemHaircut ? toBurn - redeemHaircut : 0;
        require(underlying.transfer(to, payout), "pool: push");
        return payout;
    }

    function getReserveData(address) external view override returns (ReserveData memory data) {
        data.aTokenAddress = address(aToken);
    }
}
