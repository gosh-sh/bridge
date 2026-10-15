// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title ITokenMessengerV2
/// @notice The two burn entry points of Circle's `TokenMessengerV2`.
///         `depositForBurnWithHook` appends `hookData` to the burn message;
///         Circle does not call the mint recipient. The destination contract
///         reads the hook itself after `receiveMessage` succeeds.
/// @dev Selectors match circlefin/evm-cctp-contracts `TokenMessengerV2`
///      (pragma 0.7.6). Do not add extra functions here — a wider interface
///      is not what gets called.
interface ITokenMessengerV2 {
    function depositForBurn(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold
    ) external;

    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external;
}
