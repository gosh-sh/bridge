// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title IMessageTransmitterV2
/// @notice Circle's destination entry point. Verifies the attestation, then
///         asks the local TokenMessenger to mint. Reverts if `destinationCaller`
///         is set and `msg.sender` is not that address.
interface IMessageTransmitterV2 {
    function receiveMessage(bytes calldata message, bytes calldata attestation)
        external
        returns (bool);
}
