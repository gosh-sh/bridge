// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import { Script } from "forge-std/Script.sol";
import { console } from "forge-std/console.sol";

import "./ShplonkDeployLib.sol";

/// @title PrintMultiHopCodehash
/// @notice One-shot helper that CREATE's the committed multi-hop Yul artefact
///         and logs its runtime `extcodehash`. The value is pasted into
///         `ShplonkDeployLib.MULTI_HOP_YUL_CODEHASH` so the deploy-time pin
///         mirrors the four existing verifier pins.
///
/// @dev Invocation: from `contracts/ethereum/` run
///      `forge script script/PrintMultiHopCodehash.s.sol`.
contract PrintMultiHopCodehash is Script {
    function run() external {
        address yul =
            ShplonkDeployLib.deployYulFromBin("verifiers/BridgeMultiHopAggregatorVerifier.bin");
        console.log("multi-hop yul address:", yul);
        console.logBytes32(yul.codehash);
    }
}
