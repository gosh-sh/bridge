// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IAavePool.sol";
import "../../src/IWrappedTokenGatewayV3.sol";
import "../../src/IERC20.sol";

/// @title MockAWETH
/// @notice Minimal ERC-20-like aToken with a test-only "accrue yield" hook.
/// @dev Holds real ETH 1:1 so withdrawals can actually send back native value.
contract MockAWETH is IERC20 {
    string public name = "Mock aWETH";
    string public symbol = "maWETH";
    uint8 public constant decimals = 18;

    mapping(address => uint256) internal _balances;
    mapping(address => mapping(address => uint256)) internal _allowances;
    uint256 public totalSupply;

    event Mint(address indexed to, uint256 amount);
    event Burn(address indexed from, uint256 amount);

    function balanceOf(address a) external view override returns (uint256) {
        return _balances[a];
    }

    function allowance(address o, address s) external view override returns (uint256) {
        return _allowances[o][s];
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "maWETH: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(_balances[from] >= amount, "maWETH: balance");
        unchecked {
            _balances[from] -= amount;
            _balances[to] += amount;
        }
    }

    /// @notice Mint aTokens and record incoming ETH as underlying backing.
    function mint(address to, uint256 amount) external payable {
        require(msg.value == amount, "maWETH: eth mismatch");
        _balances[to] += amount;
        totalSupply += amount;
        emit Mint(to, amount);
    }

    /// @notice Burn aTokens and forward the corresponding ETH to `to`.
    function burn(address from, uint256 amount, address to) external {
        require(_balances[from] >= amount, "maWETH: burn>balance");
        _balances[from] -= amount;
        totalSupply -= amount;
        (bool ok,) = to.call{ value: amount }("");
        require(ok, "maWETH: eth send");
        emit Burn(from, amount);
    }

    /// @notice Test-only: simulate accrued yield by minting extra aTokens to a holder.
    ///         Attach `msg.value == amount` to fund the underlying.
    function accrueYield(address to, uint256 amount) external payable {
        require(msg.value == amount, "maWETH: yield eth mismatch");
        _balances[to] += amount;
        totalSupply += amount;
    }

    receive() external payable { }
}

/// @title MockAavePool
/// @notice Test-only AAVE V3 Pool that proxies `supply`/`withdraw` to MockAWETH.
contract MockAavePool is IAavePool {
    MockAWETH public immutable aToken;

    constructor(address _aToken) {
        aToken = MockAWETH(payable(_aToken));
    }

    function supply(address, uint256 amount, address onBehalfOf, uint16) external override {
        // In real AAVE, underlying WETH is pulled from msg.sender. Our tests supply
        // via the gateway, which forwards ETH -> this contract -> aWETH.mint.
        // So we expect the incoming ETH in the pool's balance already.
        aToken.mint{ value: amount }(onBehalfOf, amount);
    }

    function withdraw(address, uint256 amount, address to) external override returns (uint256) {
        // msg.sender is the aWETH holder (either bridge directly or gateway).
        uint256 bal = aToken.balanceOf(msg.sender);
        uint256 payout = amount == type(uint256).max ? bal : amount;
        aToken.burn(msg.sender, payout, to);
        return payout;
    }

    function getReserveData(address) external view override returns (ReserveData memory data) {
        data.aTokenAddress = address(aToken);
    }

    receive() external payable { }
}

/// @title MockWETHGateway
/// @notice Test-only stand-in for AAVE's WrappedTokenGatewayV3.
contract MockWETHGateway is IWrappedTokenGatewayV3 {
    MockAavePool public immutable pool;
    MockAWETH public immutable aToken;

    constructor(address _pool, address _aToken) {
        pool = MockAavePool(payable(_pool));
        aToken = MockAWETH(payable(_aToken));
    }

    function depositETH(address, address onBehalfOf, uint16) external payable override {
        // Forward the ETH straight to the aToken mint, skipping the "real" WETH step.
        aToken.mint{ value: msg.value }(onBehalfOf, msg.value);
    }

    function withdrawETH(address, uint256 amount, address to) external override {
        // Pull aTokens from caller (bridge) using the allowance set in its constructor.
        uint256 bal = aToken.balanceOf(msg.sender);
        uint256 payout = amount == type(uint256).max ? bal : amount;
        require(bal >= payout, "gw: insufficient aWETH");
        // aToken.transferFrom requires caller-approved allowance; we use it to
        // move the aTokens to the gateway, then burn+unwrap.
        bool ok =
            MockAWETH(payable(address(aToken))).transferFrom(msg.sender, address(this), payout);
        require(ok, "gw: transferFrom");
        aToken.burn(address(this), payout, to);
    }

    receive() external payable { }
}
