// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title ReturnlessERC20
/// @notice USDT-style token: `transferFrom` returns no data on the wire.
contract ReturnlessERC20 {
    string public name = "Returnless";
    string public symbol = "RLESS";
    uint8 public constant decimals = 6;

    mapping(address => uint256) internal _balances;
    mapping(address => mapping(address => uint256)) internal _allowances;
    uint256 public totalSupply;

    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    function allowance(address owner, address spender) external view returns (uint256) {
        return _allowances[owner][spender];
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    /// @dev Deliberately omits the bool return — mirrors non-standard mainnet tokens.
    function transferFrom(address from, address to, uint256 amount) external {
        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "ReturnlessERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
    }

    function mint(address to, uint256 amount) public {
        _balances[to] += amount;
        totalSupply += amount;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(_balances[from] >= amount, "ReturnlessERC20: balance");
        unchecked {
            _balances[from] -= amount;
            _balances[to] += amount;
        }
    }
}
