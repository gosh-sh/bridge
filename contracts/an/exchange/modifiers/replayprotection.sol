pragma gosh-solidity >=0.80;

import "./errors.sol";

abstract contract BridgeReplayProtection is eccUSDCBridgeErrors {
    mapping(uint32 => mapping(uint256 => bool)) _messages;
    uint256 _messageHash;
    uint32 _messageExpire;

    modifier saveMsg() {
        _saveMsg();
        tvm.commit();
        _;
    }

    function afterSignatureCheck(TvmSlice body, TvmCell message) private inline returns (TvmSlice) {
        body.load(uint64);
        uint32 expireAt = body.load(uint32);
        require(expireAt > block.timestamp, ERR_MESSAGE_EXPIRED);
        require(expireAt < block.timestamp + 5 minutes, ERR_MESSAGE_WITH_HUGE_EXPIREAT);
        uint256 messageHash = tvm.hash(message);
        require(!_messages[expireAt][messageHash], ERR_MESSAGE_IS_EXIST);
        _messageHash = messageHash;
        _messageExpire = expireAt;
        return body;
    }

    function _saveMsg() internal {
        if (msg.isInternal) { return; }
        uint8 cleaned;
        for ((uint32 expireAt, mapping(uint256 => bool) bucket) : _messages) {
            bucket;
            if (expireAt > block.timestamp || cleaned == 20) { break; }
            delete _messages[expireAt];
            cleaned++;
        }
        _messages[_messageExpire][_messageHash] = true;
    }
}
