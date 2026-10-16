// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title CctpDomains
/// @notice Circle CCTP domain ids and the two finality thresholds TokenMessengerV2
///         accepts. Confirm the domain table against Circle's registry before a
///         deploy — these are the ids CCTP has used since v1, not chain ids.
library CctpDomains {
    uint32 internal constant ETHEREUM = 0;
    uint32 internal constant OPTIMISM = 2;
    uint32 internal constant ARBITRUM = 3;
    uint32 internal constant BASE = 6;
    uint32 internal constant POLYGON = 7;

    /// @notice Fast transfer. Circle attests before the source batch is final on
    ///         L1 and charges a fee on the destination (`feeExecuted`).
    uint32 internal constant FAST = 1000;

    /// @notice Standard transfer. Circle attests only after finality. `feeExecuted`
    ///         on a finalized message is zero when `maxFee` is zero.
    uint32 internal constant FINALIZED = 2000;
}
