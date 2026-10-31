// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IPredecessorBridge.sol";

/// @notice Stand-in for an earlier bridge: the three views a successor reads,
///         with a switch that makes `isNullifierUsed` revert.
contract MockPredecessorBridge is IPredecessorBridge {
    uint256 public immutable bridgeWithdrawalDappFr;
    uint256 public immutable bridgeWithdrawalAccFr;
    mapping(uint256 => bool) public used;
    bool public reverts;

    constructor(uint256 dappFr, uint256 accFr) {
        bridgeWithdrawalDappFr = dappFr;
        bridgeWithdrawalAccFr = accFr;
    }

    function setUsed(uint256 nullifier) external {
        used[nullifier] = true;
    }

    function setReverts(bool v) external {
        reverts = v;
    }

    function isNullifierUsed(uint256 nullifier) external view returns (bool) {
        require(!reverts, "predecessor down");
        return used[nullifier];
    }
}

/// @notice Answers `isNullifierUsed` but no identity views.
contract NullifierOnlyContract {
    function isNullifierUsed(uint256) external pure returns (bool) {
        return false;
    }
}
