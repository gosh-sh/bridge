// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./MockERC20.sol";

/// @title MultiLogERC20
/// @notice Emits extra logs on `transferFrom` — models Safe / ERC-4337 receipt spam.
/// @dev TD-11: L1 `deposit()` can succeed while deposit-prover `MAX_LOG_NUM=3` rejects.
contract MultiLogERC20 is MockERC20 {
    event AuxLog(uint256 indexed slot, bytes32 marker);
    event Transfer(address indexed from, address indexed to, uint256 value);

    constructor() MockERC20("USDC", "USDC", 6) {}

    function transferFrom(address from, address to, uint256 amount)
        public
        virtual
        override
        returns (bool)
    {
        emit AuxLog(1, bytes32(uint256(0x1111)));
        emit AuxLog(2, bytes32(uint256(0x2222)));
        emit AuxLog(3, bytes32(uint256(0x3333)));
        emit Transfer(from, to, amount);

        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "MockERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }
}
