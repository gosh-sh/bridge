// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "../src/NacklToken.sol";

contract NacklTokenTest is Test {
    address internal minter = address(0xB41D);
    address internal holder = address(0xA11CE);
    address internal spender = address(0x5EED);

    function test_NB_TOK_1_nineDecimals() public {
        NacklToken t = new NacklToken(minter, address(0), 0);
        assertEq(t.decimals(), 9);
        assertEq(t.symbol(), "NACKL");
    }

    function test_NB_TOK_2_onlyTheMinterMints() public {
        NacklToken t = new NacklToken(minter, address(0), 0);
        vm.prank(minter);
        t.mint(holder, 5);
        assertEq(t.balanceOf(holder), 5);
        vm.prank(holder);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, holder));
        t.mint(holder, 1);
    }

    function test_NB_TOK_3_holderOrApprovedBurns() public {
        NacklToken t = new NacklToken(minter, holder, 10);
        vm.prank(holder);
        t.burn(3);
        vm.prank(spender);
        vm.expectRevert();
        t.burnFrom(holder, 1);
        vm.prank(holder);
        t.approve(spender, 4);
        vm.prank(spender);
        t.burnFrom(holder, 4);
        assertEq(t.balanceOf(holder), 3);
        assertEq(t.totalSupply(), 3);
    }

    function test_NB_TOK_4_initialSupply() public {
        NacklToken prod = new NacklToken(minter, address(0), 0);
        assertEq(prod.totalSupply(), 0);
        assertEq(prod.owner(), minter);
        NacklToken test = new NacklToken(minter, holder, 1e18);
        assertEq(test.balanceOf(holder), 1e18);
    }
}
