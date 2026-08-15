// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./MockERC20.sol";
import "../../src/AckiNackiBridge.sol";

/// @title ReentrantERC20
/// @notice Reenters `deposit()` from `transferFrom` to exercise `nonReentrant`.
contract ReentrantERC20 is MockERC20 {
    AckiNackiBridge internal bridge;
    bytes32 internal reenterAccount;
    bool internal entered;

    constructor() MockERC20("R", "R", 6) {}

    function wireReenter(AckiNackiBridge _bridge, bytes32 anAccount) external {
        bridge = _bridge;
        reenterAccount = anAccount;
        entered = false;
    }

    function transferFrom(address from, address to, uint256 amount)
        external
        override
        returns (bool)
    {
        if (!entered && msg.sender == address(bridge) && to == address(bridge)) {
            entered = true;
            bridge.deposit(amount, int8(0), reenterAccount);
        }

        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "ReentrantERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }
}
