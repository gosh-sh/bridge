// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IERC20 (minimal)
/// @notice Trimmed ERC-20 interface — only the calls the bridge needs for aToken custody.
interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
}
