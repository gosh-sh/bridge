// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title AnDestinationHook
/// @notice Versioned hook carried inside a CCTP burn message.
///         Version 1 is `abi.encode(uint8 version, int8 anWorkchain, bytes32 anAccount, address depositor)`.
///         `anWorkchain` is inert on Acki Nacki (the bridge keeps it only so the
///         `Deposit` event shape stays stable). `anAccount` is the 256-bit TVM
///         recipient the hub bridge puts in that event. `depositor` is the
///         caller on the source chain, for provenance — the `Deposit` event's
///         `sender` is the hub receiver, because that is who calls `deposit`.
library AnDestinationHook {
    uint8 internal constant VERSION = 1;

    /// @dev `abi.encode` of the four fields. A different length is a different
    ///      version, not a truncated v1, and is rejected.
    uint256 internal constant ENCODED_LENGTH = 128;

    error BadHook();
    error BadHookVersion(uint8 version);

    function encode(int8 anWorkchain, bytes32 anAccount, address depositor)
        internal
        pure
        returns (bytes memory)
    {
        if (anAccount == bytes32(0)) revert BadHook();
        return abi.encode(VERSION, anWorkchain, anAccount, depositor);
    }

    function decode(bytes memory hook)
        internal
        pure
        returns (int8 anWorkchain, bytes32 anAccount, address depositor)
    {
        if (hook.length != ENCODED_LENGTH) revert BadHook();
        uint8 version;
        (version, anWorkchain, anAccount, depositor) =
            abi.decode(hook, (uint8, int8, bytes32, address));
        if (version != VERSION) revert BadHookVersion(version);
        if (anAccount == bytes32(0)) revert BadHook();
    }
}
