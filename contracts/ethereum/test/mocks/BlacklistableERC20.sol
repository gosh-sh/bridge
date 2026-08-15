// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./MockERC20.sol";

/// @title BlacklistableERC20
/// @notice USDC-style mock: global pause + per-account blacklist on transfers.
contract BlacklistableERC20 is MockERC20 {
    bool public paused;
    mapping(address => bool) public isBlacklisted;

    constructor() MockERC20("bUSDC", "bUSDC", 6) {}

    function setPaused(bool value) external {
        paused = value;
    }

    function blacklist(address account) external {
        isBlacklisted[account] = true;
    }

    function unblacklist(address account) external {
        isBlacklisted[account] = false;
    }

    function transferFrom(address from, address to, uint256 amount)
        external
        override
        returns (bool)
    {
        _checkTransfer(from, to);
        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "BlacklistableERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _checkTransfer(address from, address to) internal view {
        if (paused) revert Paused();
        if (isBlacklisted[from] || isBlacklisted[to]) revert Blacklisted();
    }

    error Paused();
    error Blacklisted();
}
