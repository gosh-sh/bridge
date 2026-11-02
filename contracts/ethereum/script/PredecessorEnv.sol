// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import { Vm } from "forge-std/Vm.sol";

/// @title PredecessorEnv
/// @notice `PREDECESSOR_BRIDGE` for every script that deploys an `AckiNackiBridge`
///         to a live chain: the bridge whose treasury the new one may take
///         over (`migrateTreasury` / `fundTreasury`). Optional, zero by
///         default; replay between bridges is prevented by the proof's
///         `dstBridge`, not by this field.
library PredecessorEnv {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function read() internal view returns (address) {
        return VM.envOr("PREDECESSOR_BRIDGE", address(0));
    }
}
