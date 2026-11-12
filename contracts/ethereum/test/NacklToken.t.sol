// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { IERC20Errors } from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";

import { NacklToken } from "../src/NacklToken.sol";
import { DeployNacklToken } from "../script/DeployNacklToken.s.sol";

/// @title NacklTokenTest
/// @notice ERC20 NACKL and its deploy script; TOK-1 … TOK-5 of
///         docs/NACKL-bridge-extension.md.
contract NacklTokenTest is Test {
    address internal bridge = address(0xB1D9E);
    address internal holder = address(0xA11CE);
    address internal spender = address(0x5BE7D);

    /// @notice TOK-1, TOK-3: 9 decimals, the owner from the constructor, no supply.
    function test_TOK1_TOK3_nineDecimalsAndNoSupply() public {
        NacklToken t = new NacklToken(bridge, address(0), 0);
        assertEq(t.decimals(), 9);
        assertEq(t.owner(), bridge);
        assertEq(t.totalSupply(), 0);
        assertEq(t.symbol(), "NACKL");
    }

    /// @notice TOK-2: only the owner mints; holders burn their own and what they may spend.
    function test_TOK2_onlyTheOwnerMintsAndHoldersBurn() public {
        NacklToken t = new NacklToken(bridge, address(0), 0);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, holder));
        vm.prank(holder);
        t.mint(holder, 1);

        vm.prank(bridge);
        t.mint(holder, 100);
        vm.prank(holder);
        t.burn(30);
        assertEq(t.totalSupply(), 70);

        vm.prank(spender);
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientAllowance.selector, spender, 0, 10)
        );
        t.burnFrom(holder, 10);
        vm.prank(holder);
        t.approve(spender, 10);
        vm.prank(spender);
        t.burnFrom(holder, 10);
        assertEq(t.balanceOf(holder), 60);
    }

    /// @notice TOK-5: a test token starts with its supply at the holder.
    function test_TOK5_testTokenStartsWithASupply() public {
        NacklToken t = new NacklToken(bridge, holder, 1_000_000e9);
        assertEq(t.balanceOf(holder), 1_000_000e9);
        assertEq(t.totalSupply(), 1_000_000e9);
    }

    /// @notice TOK-4, TOK-5: the deploy script refuses a supply outside a test token and an
    ///         owner that is not a contract.
    function test_TOK4_deployScriptRefusesASupplyOutsideATestToken() public {
        DeployNacklToken script = new DeployNacklToken();
        address deployedBridge = address(new NacklToken(address(this), address(0), 0));

        vm.expectRevert(bytes("only a test token (NACKL_TEST_TOKEN=1) starts with a supply"));
        script.deploy(deployedBridge, false, holder, 1);
        vm.expectRevert(bytes("only a test token (NACKL_TEST_TOKEN=1) starts with a supply"));
        script.deploy(deployedBridge, false, holder, 0);
        vm.expectRevert(bytes("NACKL_BRIDGE must be a deployed bridge"));
        script.deploy(address(0xE0A), false, address(0), 0);
        vm.expectRevert(bytes("NACKL_INITIAL_HOLDER required with a supply"));
        script.deploy(deployedBridge, true, address(0), 1);

        NacklToken production = script.deploy(deployedBridge, false, address(0), 0);
        assertEq(production.totalSupply(), 0);
        assertEq(production.owner(), deployedBridge);
        NacklToken test = script.deploy(deployedBridge, true, holder, 5e9);
        assertEq(test.balanceOf(holder), 5e9);
        assertEq(test.owner(), deployedBridge);
    }
}
