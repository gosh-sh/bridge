// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title ReentrancyGuard
/// @notice Single-slot guard. Uninitialised storage is not `_ENTERED`, so the
///         first call does not need a constructor write.
abstract contract ReentrancyGuard {
    uint256 private constant _ENTERED = 2;

    uint256 private _status;

    error Reentrancy();

    modifier nonReentrant() {
        if (_status == _ENTERED) revert Reentrancy();
        _status = _ENTERED;
        _;
        _status = 1;
    }
}
