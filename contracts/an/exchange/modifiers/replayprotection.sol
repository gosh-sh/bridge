pragma gosh-solidity >=0.80;

import "./errors.sol";

/// @notice Hash-and-expiry replay protection adapted from the DEX and
///         multifactor contracts. Public messages never advance a shared clock.
abstract contract BridgeReplayProtection is eccUSDCBridgeErrors {
    mapping(uint32 => mapping(uint256 => bool)) _replayMessages;
    // Retire signed messages already covered by the previous compiler guard.
    // This floor is set only at the legacy upgrade, never by public deposits.
    uint64 _legacyReplayTimeFloor;
    uint8 constant MAX_CLEANUP_ITERATIONS = 20;

    struct ReplayMessage {
        uint256 messageHash;
        uint32 expireAt;
    }
    ReplayMessage _lastReplayMessage;

    /// @dev Use after owner authorization and accept, only for external calls.
    modifier saveMsg() {
        _saveMsg();
        tvm.commit();
        _;
    }

    /// @dev Internal callbacks have no external message header to record.
    function _saveMsg() internal {
        if (msg.isInternal) { return; }
        uint8 count = 0;
        for ((uint32 expireAt, mapping(uint256 => bool) bucket) : _replayMessages) {
            bucket;
            if (count >= MAX_CLEANUP_ITERATIONS || expireAt > block.timestamp) { break; }
            count++;
            delete _replayMessages[expireAt];
        }
        _replayMessages[_lastReplayMessage.expireAt][_lastReplayMessage.messageHash] = true;
    }

    /// @dev The compiler calls this instead of its timestamp replay guard.
    ///      Signature verification still runs before this hook.
    function afterSignatureCheck(TvmSlice body, TvmCell message) private inline returns (TvmSlice) {
        uint64 messageTime = body.load(uint64);
        uint32 expireAt = body.load(uint32);
        require(msg.pubkey() == 0 || messageTime > _legacyReplayTimeFloor,
                ERR_MESSAGE_BEFORE_UPGRADE);
        require(expireAt > block.timestamp, ERR_MESSAGE_EXPIRED);
        require(expireAt < block.timestamp + 5 minutes, ERR_MESSAGE_WITH_HUGE_EXPIREAT);
        uint256 messageHash = tvm.hash(message);
        require(!_replayMessages[expireAt][messageHash], ERR_MESSAGE_IS_EXIST);
        _lastReplayMessage = ReplayMessage(messageHash, expireAt);
        return body;
    }
}
