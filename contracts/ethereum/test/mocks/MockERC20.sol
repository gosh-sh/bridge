// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IERC20.sol";

/// @title MockERC20
/// @notice Test-only ERC-20 with configurable decimals and a public `mint`.
contract MockERC20 is IERC20 {
    string public name;
    string public symbol;
    uint8 public immutable decimals;

    mapping(address => uint256) internal _balances;
    mapping(address => mapping(address => uint256)) internal _allowances;
    uint256 public totalSupply;

    constructor(string memory _name, string memory _symbol, uint8 _decimals) {
        name = _name;
        symbol = _symbol;
        decimals = _decimals;
    }

    function balanceOf(address account) external view override returns (uint256) {
        return _balances[account];
    }

    function allowance(address owner, address spender) external view override returns (uint256) {
        return _allowances[owner][spender];
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount)
        external
        virtual
        override
        returns (bool)
    {
        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "MockERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function mint(address to, uint256 amount) public {
        _balances[to] += amount;
        totalSupply += amount;
    }

    function _burn(address from, uint256 amount) internal {
        require(_balances[from] >= amount, "MockERC20: burn");
        unchecked {
            _balances[from] -= amount;
            totalSupply -= amount;
        }
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(_balances[from] >= amount, "MockERC20: balance");
        unchecked {
            _balances[from] -= amount;
            _balances[to] += amount;
        }
    }
}
