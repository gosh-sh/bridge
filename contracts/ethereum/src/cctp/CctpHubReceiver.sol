// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../IERC20.sol";
import "./AnDestinationHook.sol";
import "./CctpV2Message.sol";
import "./IDepositBridge.sol";
import "./IMessageTransmitterV2.sol";
import "./ReentrancyGuard.sol";
import "./TwoStepOwnable.sol";

/// @title CctpHubReceiver
/// @notice Sits on the hub next to one `AckiNackiBridge` and therefore one token.
///
///         Circle mints to `mintRecipient` and stops there — the hook is not
///         executed. This contract is both the mint recipient and the message's
///         `destinationCaller`, so the mint and `bridge.deposit` happen in one
///         transaction: `receiveAndDeposit` parses the burn, asks the
///         MessageTransmitter to verify the attestation and mint, and deposits
///         the amount that actually arrived (`amount - feeExecuted`).
///
///         Sources are a registry, not a hard-coded chain. Arbitrum is one row
///         (`domain => adapter, remote token, EIP-155 chain id`). A second
///         domain or a second token (another receiver, another bridge) does not
///         change this code.
///
///         The `Deposit` event this produces is a hub deposit. Its `chainId` is
///         the hub's, its `sender` is this contract, and its `anAccount` comes
///         from the hook. Acki Nacki trusts the hub bridge, not the remote
///         chain. The remote chain id is in `RemoteDepositForwarded` for the
///         indexer; it is not a public input of the deposit proof.
contract CctpHubReceiver is TwoStepOwnable, ReentrancyGuard {
    struct Source {
        uint256 chainId;
        bytes32 adapter;
        bytes32 remoteToken;
        bool enabled;
    }

    IDepositBridge public immutable bridge;
    IERC20 public immutable token;
    IMessageTransmitterV2 public immutable messageTransmitter;
    uint32 public immutable localDomain;

    mapping(uint32 domain => Source) public sources;

    event SourceSet(
        uint32 indexed domain, uint256 chainId, bytes32 adapter, bytes32 remoteToken, bool enabled
    );
    event RemoteDepositForwarded(
        uint32 indexed sourceDomain,
        uint256 indexed chainId,
        address indexed depositor,
        bytes32 anAccount,
        uint256 amount,
        uint256 feeExecuted
    );
    event Rescued(address indexed token, address indexed to, uint256 amount);

    error IncompleteSource();
    error UnexpectedDomain(uint32 domain);
    error UnexpectedCaller();
    error SourceDisabled(uint32 domain);
    error UnexpectedToken();
    error UnexpectedAdapter();
    error UnexpectedRecipient();
    error FeeExceedsAmount();
    error ZeroMint();
    error ReceiveFailed();
    error MintMismatch(uint256 expected, uint256 actual);
    error TransferFailed();

    constructor(address bridge_, address messageTransmitter_, uint32 localDomain_) {
        if (bridge_ == address(0) || messageTransmitter_ == address(0)) revert ZeroAddress();
        bridge = IDepositBridge(bridge_);
        address token_ = bridge.usdc();
        if (token_ == address(0)) revert ZeroAddress();
        token = IERC20(token_);
        messageTransmitter = IMessageTransmitterV2(messageTransmitter_);
        localDomain = localDomain_;
    }

    /// @notice Trust burns from `adapter` of `remoteToken` on CCTP `domain`.
    ///         `chainId` is the EIP-155 id recorded in `RemoteDepositForwarded`.
    ///         Enabling requires all three of adapter, remote token and chain id.
    function setSource(
        uint32 domain,
        uint256 chainId,
        bytes32 adapter,
        bytes32 remoteToken,
        bool enabled
    ) external onlyOwner {
        if (enabled && (adapter == bytes32(0) || remoteToken == bytes32(0) || chainId == 0)) {
            revert IncompleteSource();
        }
        sources[domain] = Source({
            chainId: chainId, adapter: adapter, remoteToken: remoteToken, enabled: enabled
        });
        emit SourceSet(domain, chainId, adapter, remoteToken, enabled);
    }

    /// @notice Verify `message` with Circle and deposit the minted tokens.
    ///         Permissionless: the attestation is the authorization, and the
    ///         message names this contract as `destinationCaller`, so no one
    ///         else can mint it out from under us.
    function receiveAndDeposit(bytes calldata message, bytes calldata attestation)
        external
        nonReentrant
        returns (uint256 minted)
    {
        CctpV2Message.Parsed memory parsed = CctpV2Message.parse(message);
        if (parsed.destinationDomain != localDomain) {
            revert UnexpectedDomain(parsed.destinationDomain);
        }
        if (parsed.destinationCaller != _asBytes32(address(this))) revert UnexpectedCaller();

        Source memory source = sources[parsed.sourceDomain];
        if (!source.enabled) revert SourceDisabled(parsed.sourceDomain);
        if (parsed.burnToken != source.remoteToken) revert UnexpectedToken();
        if (parsed.messageSender != source.adapter) revert UnexpectedAdapter();
        if (parsed.mintRecipient != _asBytes32(address(this))) revert UnexpectedRecipient();
        if (parsed.feeExecuted > parsed.amount) revert FeeExceedsAmount();

        (int8 anWorkchain, bytes32 anAccount, address depositor) =
            AnDestinationHook.decode(parsed.hookData);

        minted = parsed.amount - parsed.feeExecuted;
        if (minted == 0) revert ZeroMint();

        uint256 beforeBal = token.balanceOf(address(this));
        if (!messageTransmitter.receiveMessage(message, attestation)) revert ReceiveFailed();
        uint256 afterBal = token.balanceOf(address(this));
        if (afterBal < beforeBal || afterBal - beforeBal != minted) {
            revert MintMismatch(minted, afterBal - beforeBal);
        }

        _approve(address(bridge), 0);
        _approve(address(bridge), minted);
        bridge.deposit(minted, anWorkchain, anAccount);
        if (token.allowance(address(this), address(bridge)) != 0) {
            _approve(address(bridge), 0);
        }

        emit RemoteDepositForwarded(
            parsed.sourceDomain, source.chainId, depositor, anAccount, minted, parsed.feeExecuted
        );
    }

    /// @notice Pull tokens that were sent here outside `receiveAndDeposit`.
    ///         A successful receive leaves a zero balance: the mint and the
    ///         deposit are one transaction, and a failing deposit rolls the
    ///         mint back with it.
    function rescue(address token_, address to, uint256 amount) external onlyOwner nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        if (!IERC20(token_).transfer(to, amount)) revert TransferFailed();
        emit Rescued(token_, to, amount);
    }

    function _approve(address spender, uint256 amount) internal {
        if (!token.approve(spender, amount)) revert TransferFailed();
    }

    function _asBytes32(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}
