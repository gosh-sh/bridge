// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IAavePool.sol";
import "../../src/IERC20.sol";
import "./MockERC20.sol";

/// @title MockAUSDT
/// @notice Minimal 6-decimal aToken with test-only mint/burn hooks.
contract MockAUSDT is MockERC20 {
    constructor() MockERC20("Mock aUSDT", "maUSDT", 6) { }

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
/// @notice Test-only AAVE V3 Pool that proxies ERC-20 `supply`/`withdraw` to MockAUSDT.
contract MockAavePool is IAavePool {
    IERC20 public immutable underlying;
    MockAUSDT public immutable aToken;

    constructor(address _underlying, address _aToken) {
        underlying = IERC20(_underlying);
        aToken = MockAUSDT(_aToken);
    }

    function supply(address, uint256 amount, address onBehalfOf, uint16) external override {
        require(underlying.transferFrom(msg.sender, address(this), amount), "pool: pull");
        aToken.mintTo(onBehalfOf, amount);
    }

    function withdraw(address, uint256 amount, address to) external override returns (uint256) {
        uint256 bal = aToken.balanceOf(msg.sender);
        uint256 payout = amount == type(uint256).max ? bal : amount;
        require(bal >= payout, "pool: insufficient aUSDT");
        aToken.burnFrom(msg.sender, payout);
        require(underlying.transfer(to, payout), "pool: push");
        return payout;
    }

    function getReserveData(address) external view override returns (ReserveData memory data) {
        data.aTokenAddress = address(aToken);
    }
}
