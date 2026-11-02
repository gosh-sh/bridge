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

    /// @notice `LEGACY_PREDECESSOR_BRIDGES`: comma-separated earlier bridges for
    ///         the same `WITHDRAW_ACC_FR` that `PREDECESSOR_BRIDGE` does not
    ///         answer for. Required, possibly empty, when `PREDECESSOR_BRIDGE`
    ///         predates the `predecessor` field: such a bridge answers only for
    ///         its own nullifiers, so every bridge before it has to be listed.
    function readLegacy() internal view returns (IPredecessorBridge[] memory legacy) {
        address predecessor = VM.envAddress("PREDECESSOR_BRIDGE");
        if (predecessor != address(0) && !_answersForItsLine(predecessor)) {
            require(
                VM.envExists("LEGACY_PREDECESSOR_BRIDGES"),
                "PREDECESSOR_BRIDGE predates the predecessor field: set LEGACY_PREDECESSOR_BRIDGES to every earlier bridge before it, empty if none"
            );
        }
        address[] memory addrs = VM.envOr("LEGACY_PREDECESSOR_BRIDGES", ",", new address[](0));
        legacy = new IPredecessorBridge[](addrs.length);
        for (uint256 i = 0; i < addrs.length; ++i) {
            require(
                AckiNackiBridge(addrs[i]).paused(),
                "pause every LEGACY_PREDECESSOR_BRIDGES entry before the deploy"
            );
            legacy[i] = IPredecessorBridge(addrs[i]);
        }
    }

    function _answersForItsLine(address bridge) private view returns (bool) {
        try AckiNackiBridge(bridge).predecessor() returns (IPredecessorBridge) {
            return true;
        } catch {
            return false;
        }
    }
}
