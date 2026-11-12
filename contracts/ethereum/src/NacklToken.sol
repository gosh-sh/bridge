// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import { ERC20Burnable } from "@openzeppelin/contracts/token/ERC20/extensions/ERC20Burnable.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";

/// @title ERC20 NACKL
/// @notice NACKL, the Acki Nacki native token, on an EVM chain. Its owner — the
///         `AckiNackiBridge` that pays NACKL out — is the only account that
///         mints it, against NACKL locked on Acki Nacki; `deposit` burns it.
/// @dev 9 decimals, as NACKL has on Acki Nacki: the bridge carries amounts
///      unchanged. A token a bridge pays out on a public network is deployed
///      with no initial supply; `initialSupply` is for test tokens.
contract NacklToken is ERC20, ERC20Burnable, Ownable {
    constructor(address owner_, address initialHolder, uint256 initialSupply)
        ERC20("NACKL", "NACKL")
        Ownable(owner_)
    {
        if (initialSupply != 0) _mint(initialHolder, initialSupply);
    }

    function decimals() public pure override returns (uint8) {
        return 9;
    }

    function mint(address to, uint256 amount) external onlyOwner {
        _mint(to, amount);
    }
}
