// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./MockERC20.sol";

/// @title FeeOnTransferERC20
/// @notice Mock token that delivers less than the requested `transferFrom` amount.
contract FeeOnTransferERC20 is MockERC20 {
    uint256 public feeBps;

    constructor(string memory _name, string memory _symbol, uint8 _decimals, uint256 _feeBps)
        MockERC20(_name, _symbol, _decimals)
    {
        feeBps = _feeBps;
    }

    function transferFrom(address from, address to, uint256 amount)
        external
        override
        returns (bool)
    {
        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "FeeOnTransferERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        uint256 fee = (amount * feeBps) / 10_000;
        uint256 net = amount - fee;
        _transfer(from, to, net);
        return true;
    }
}
