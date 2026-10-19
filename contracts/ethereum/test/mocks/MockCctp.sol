// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/IERC20.sol";
import "../../src/cctp/CctpV2Message.sol";
import "../../src/cctp/IMessageTransmitterV2.sol";
import "../../src/cctp/ITokenMessengerV2.sol";
import "./MockERC20.sol";

/// @title MockTokenMessenger
/// @notice Records Circle's burn calls and pulls the tokens, the way
///         `TokenMessengerV2` does. Does not emit a real CCTP message — the
///         test builds that from the recorded arguments.
contract MockTokenMessenger is ITokenMessengerV2 {
    address public lastBurnToken;
    uint256 public lastAmount;
    uint32 public lastDestinationDomain;
    bytes32 public lastMintRecipient;
    bytes32 public lastDestinationCaller;
    uint256 public lastMaxFee;
    uint32 public lastMinFinality;
    bytes public lastHook;
    bool public lastWithHook;
    uint256 public calls;

    function depositForBurn(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold
    ) external {
        _pull(burnToken, amount);
        lastHook = "";
        lastWithHook = false;
        _record(
            amount,
            destinationDomain,
            mintRecipient,
            burnToken,
            destinationCaller,
            maxFee,
            minFinalityThreshold
        );
    }

    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external {
        require(hookData.length > 0, "Hook data is empty");
        _pull(burnToken, amount);
        lastHook = hookData;
        lastWithHook = true;
        _record(
            amount,
            destinationDomain,
            mintRecipient,
            burnToken,
            destinationCaller,
            maxFee,
            minFinalityThreshold
        );
    }

    function _record(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold
    ) internal {
        calls += 1;
        lastAmount = amount;
        lastDestinationDomain = destinationDomain;
        lastMintRecipient = mintRecipient;
        lastBurnToken = burnToken;
        lastDestinationCaller = destinationCaller;
        lastMaxFee = maxFee;
        lastMinFinality = minFinalityThreshold;
    }

    function _pull(address burnToken, uint256 amount) internal {
        require(IERC20(burnToken).transferFrom(msg.sender, address(this), amount), "pull");
    }
}

/// @title MockMessageTransmitter
/// @notice Mints the hub token mapped from the burn message's remote token,
///         and only when the outer `recipient` is the local TokenMessenger —
///         the way Circle's transmitter dispatches to `TokenMessengerV2`.
///         A generic `sendMessage` therefore does not mint. `mintDelta` lets
///         a test mint a different amount than `amount - fee` and check the
///         receiver rejects it.
contract MockMessageTransmitter is IMessageTransmitterV2 {
    mapping(bytes32 remoteToken => address localToken) public localToken;
    mapping(bytes32 nonce => bool) public usedNonce;
    bytes32 public localMessenger;
    int256 public mintDelta;
    uint256 public calls;

    error NonceUsed();
    error UnknownRemoteToken();

    function setLocalToken(bytes32 remoteToken, address local) external {
        localToken[remoteToken] = local;
    }

    function setLocalMessenger(address messenger) external {
        localMessenger = bytes32(uint256(uint160(messenger)));
    }

    function setMintDelta(int256 delta) external {
        mintDelta = delta;
    }

    function receiveMessage(bytes calldata message, bytes calldata) external returns (bool) {
        CctpV2Message.Parsed memory parsed = CctpV2Message.parse(message);
        if (usedNonce[parsed.nonce]) revert NonceUsed();
        usedNonce[parsed.nonce] = true;
        calls += 1;

        if (localMessenger != bytes32(0) && parsed.recipient != localMessenger) {
            return true;
        }

        address local = localToken[parsed.burnToken];
        if (local == address(0)) revert UnknownRemoteToken();

        uint256 net = parsed.amount - parsed.feeExecuted;
        int256 adjusted = int256(net) + mintDelta;
        require(adjusted > 0, "mint");
        MockERC20(local).mint(address(uint160(uint256(parsed.mintRecipient))), uint256(adjusted));
        return true;
    }
}
