// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Burnable.sol";
import "@openzeppelin/contracts/access/Ownable.sol";

contract NacklToken is ERC20, ERC20Burnable, Ownable {
    constructor(address minter, address initialHolder, uint256 initialSupply)
        ERC20("Acki Nacki NACKL", "NACKL")
        Ownable(minter)
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
