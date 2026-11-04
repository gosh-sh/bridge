// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IPredecessorBridge.sol";

/// @notice Stand-in for an earlier bridge: the identity views a successor
///         reads before it takes over the treasury.
contract MockPredecessorBridge is IPredecessorBridge {
    uint256 public immutable bridgeWithdrawalDappFr;
    uint256 public immutable bridgeWithdrawalAccFr;

    constructor(uint256 dappFr, uint256 accFr) {
        bridgeWithdrawalDappFr = dappFr;
        bridgeWithdrawalAccFr = accFr;
    }
}

/// @notice Answers `bridgeWithdrawalDappFr` but not `bridgeWithdrawalAccFr`.
contract DappFrOnlyContract {
    uint256 public immutable bridgeWithdrawalDappFr;

    constructor(uint256 dappFr) {
        bridgeWithdrawalDappFr = dappFr;
    }
}
