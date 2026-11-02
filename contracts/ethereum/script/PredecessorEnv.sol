// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import { Vm } from "forge-std/Vm.sol";

import "../src/AckiNackiBridge.sol";
import "../src/IPredecessorBridge.sol";

/// @title PredecessorEnv
/// @notice `PREDECESSOR_BRIDGE` for every script that deploys an `AckiNackiBridge`
///         to a live chain.
/// @dev No default: a bridge deployed with `predecessor = 0` in place of an
///      existing one pays again every withdrawal the old one already paid. The
///      first bridge on a chain sets the zero address explicitly. The
///      predecessor does not consult its successor's nullifiers, so it must be
///      paused before the successor exists.
library PredecessorEnv {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function read() internal view returns (address predecessor) {
        predecessor = VM.envAddress("PREDECESSOR_BRIDGE");
        if (predecessor != address(0)) {
            require(
                AckiNackiBridge(predecessor).paused(), "pause PREDECESSOR_BRIDGE before the deploy"
            );
        }
    }
}
