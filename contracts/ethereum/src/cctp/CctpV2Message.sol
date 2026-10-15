// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title CctpV2Message
/// @notice Encode and decode a CCTP V2 relay message whose body is a
///         `BurnMessageV2`. Layout is Circle's, not ours:
///
///         outer message (MessageV2), bytes:
///           version 0, sourceDomain 4, destinationDomain 8, nonce 12,
///           sender 44, recipient 76, destinationCaller 108,
///           minFinalityThreshold 140, finalityThresholdExecuted 144,
///           messageBody 148
///
///         message body (BurnMessageV2), relative to byte 148:
///           version 0, burnToken 4, mintRecipient 36, amount 68,
///           messageSender 100, maxFee 132, feeExecuted 164,
///           expirationBlock 196, hookData 228
///
///         `messageSender` is the source account that called `depositForBurn`
///         (our endpoint). `sender` on the outer message is the remote
///         TokenMessenger. Circle fills `nonce`, `feeExecuted`,
///         `expirationBlock` and `finalityThresholdExecuted` when it attests;
///         a message built on the source has those zero.
library CctpV2Message {
    uint32 internal constant VERSION = 1;

    uint256 internal constant BODY_OFFSET = 148;
    uint256 internal constant HOOK_OFFSET = 376;

    error MessageTooShort();
    error UnsupportedVersion(uint32 version);

    struct Relay {
        uint32 sourceDomain;
        uint32 destinationDomain;
        bytes32 nonce;
        bytes32 sender;
        bytes32 recipient;
        bytes32 destinationCaller;
        uint32 minFinalityThreshold;
        uint32 finalityThresholdExecuted;
        bytes32 burnToken;
        bytes32 mintRecipient;
        uint256 amount;
        bytes32 messageSender;
        uint256 maxFee;
        uint256 feeExecuted;
        uint256 expirationBlock;
        bytes hookData;
    }

    struct Parsed {
        uint32 version;
        uint32 sourceDomain;
        uint32 destinationDomain;
        bytes32 nonce;
        bytes32 sender;
        bytes32 recipient;
        bytes32 destinationCaller;
        uint32 minFinalityThreshold;
        uint32 finalityThresholdExecuted;
        uint32 burnVersion;
        bytes32 burnToken;
        bytes32 mintRecipient;
        uint256 amount;
        bytes32 messageSender;
        uint256 maxFee;
        uint256 feeExecuted;
        uint256 expirationBlock;
        bytes hookData;
    }

    function format(Relay memory relay) internal pure returns (bytes memory) {
        bytes memory head = abi.encodePacked(
            VERSION,
            relay.sourceDomain,
            relay.destinationDomain,
            relay.nonce,
            relay.sender,
            relay.recipient,
            relay.destinationCaller,
            relay.minFinalityThreshold,
            relay.finalityThresholdExecuted
        );
        bytes memory body = abi.encodePacked(
            VERSION,
            relay.burnToken,
            relay.mintRecipient,
            relay.amount,
            relay.messageSender,
            relay.maxFee,
            relay.feeExecuted,
            relay.expirationBlock,
            relay.hookData
        );
        return bytes.concat(head, body);
    }

    function parse(bytes memory message) internal pure returns (Parsed memory parsed) {
        if (message.length < HOOK_OFFSET) revert MessageTooShort();

        parsed.version = _uint32(message, 0);
        if (parsed.version != VERSION) revert UnsupportedVersion(parsed.version);
        parsed.sourceDomain = _uint32(message, 4);
        parsed.destinationDomain = _uint32(message, 8);
        parsed.nonce = _word(message, 12);
        parsed.sender = _word(message, 44);
        parsed.recipient = _word(message, 76);
        parsed.destinationCaller = _word(message, 108);
        parsed.minFinalityThreshold = _uint32(message, 140);
        parsed.finalityThresholdExecuted = _uint32(message, 144);

        parsed.burnVersion = _uint32(message, BODY_OFFSET);
        if (parsed.burnVersion != VERSION) revert UnsupportedVersion(parsed.burnVersion);
        parsed.burnToken = _word(message, BODY_OFFSET + 4);
        parsed.mintRecipient = _word(message, BODY_OFFSET + 36);
        parsed.amount = uint256(_word(message, BODY_OFFSET + 68));
        parsed.messageSender = _word(message, BODY_OFFSET + 100);
        parsed.maxFee = uint256(_word(message, BODY_OFFSET + 132));
        parsed.feeExecuted = uint256(_word(message, BODY_OFFSET + 164));
        parsed.expirationBlock = uint256(_word(message, BODY_OFFSET + 196));
        parsed.hookData = _tail(message, HOOK_OFFSET);
    }

    function _word(bytes memory data, uint256 offset) private pure returns (bytes32 word) {
        if (data.length < offset + 32) revert MessageTooShort();
        assembly ("memory-safe") {
            word := mload(add(add(data, 32), offset))
        }
    }

    function _uint32(bytes memory data, uint256 offset) private pure returns (uint32) {
        if (data.length < offset + 4) revert MessageTooShort();
        bytes32 word;
        assembly ("memory-safe") {
            word := mload(add(add(data, 32), offset))
        }
        return uint32(uint256(word) >> 224);
    }

    function _tail(bytes memory data, uint256 offset) private pure returns (bytes memory out) {
        uint256 len = data.length - offset;
        out = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            out[i] = data[offset + i];
        }
    }
}
