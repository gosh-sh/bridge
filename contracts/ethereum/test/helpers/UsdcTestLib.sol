// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Vm.sol";
import "../mocks/MockERC20.sol";
import "../../src/AckiNackiBridge.sol";

/// @title UsdcTestLib
/// @notice Shared helpers for USDC-denominated bridge tests (6 decimals).
library UsdcTestLib {
    /// @dev One USDC base unit (USDC has 6 decimals on Ethereum).
    uint256 internal constant UNIT = 1e6;

    /// @dev Mint `amount` mock USDC to `user`, approve the bridge, and deposit
    ///      to a default Acki Nacki recipient (workchain 0, account derived
    ///      from `user` so it is deterministic and non-zero).
    function depositUsdc(
        Vm vm,
        MockERC20 usdc,
        AckiNackiBridge bridge,
        address user,
        uint256 amount
    ) internal {
        depositUsdcTo(vm, usdc, bridge, user, amount, int8(0), bytes32(uint256(uint160(user))));
    }

    /// @dev As `depositUsdc` but with an explicit Acki Nacki destination.
    function depositUsdcTo(
        Vm vm,
        MockERC20 usdc,
        AckiNackiBridge bridge,
        address user,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount
    ) internal {
        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, anWorkchain, anAccount);
        vm.stopPrank();
    }
}
