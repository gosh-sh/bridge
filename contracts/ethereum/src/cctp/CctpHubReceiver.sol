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
///         `destinationCaller`. Crediting takes two transactions:
///
///         1. `receiveBurn(message, attestation)` — permissionless. Parses the
///            burn, asks the MessageTransmitter to verify and mint, and parks
///            `amount - feeExecuted` under the message key.
///         2. `forward(key)` — a configured forwarder or the depositor calls
///            `bridge.deposit` for the parked amount.
///
///         The `Deposit` from step 2 is what Acki Nacki proves. The deposit
///         circuit accepts only a type 1 or type 2 transaction whose receipt
///         fits its keccak budget. The Circle attestation is public once Iris
///         signs it, so if one permissionless call minted and deposited, anyone
///         could send it as a legacy (type 0) or EIP-7702 transaction and leave
///         a deposit no one can prove. Minting is harmless — it only parks the
///         funds here — so step 1 stays open, and step 2, which the contract
///         cannot type-check, is limited to forwarders and the depositor.
///         `forward` is 36 bytes of calldata, and its receipt holds the token
///         `Transfer` and the `Deposit`.
///
///         Sources are a registry, not a hard-coded chain. Arbitrum is one row
///         (`domain => adapter, remote token, EIP-155 chain id`). A second
///         domain or a second token (another receiver, another bridge) does not
///         change this code.
///
///         The `Deposit` event is a hub deposit. Its `chainId` is the hub's, its
///         `sender` is this contract, and its `anAccount` comes from the hook.
///         Acki Nacki trusts the hub bridge, not the remote chain. The remote
///         chain id is in `RemoteDepositReceived`; it is not a public input of
///         the deposit proof.
contract CctpHubReceiver is TwoStepOwnable, ReentrancyGuard {
    struct Source {
        uint256 chainId;
        bytes32 adapter;
        bytes32 remoteToken;
        bool enabled;
    }

    struct Pending {
        uint256 amount;
        bytes32 anAccount;
        address depositor;
        int8 anWorkchain;
    }

    IDepositBridge public immutable bridge;
    IERC20 public immutable token;
    IMessageTransmitterV2 public immutable messageTransmitter;
    uint32 public immutable localDomain;

    mapping(uint32 domain => Source) public sources;
    mapping(bytes32 key => Pending) public pending;
    mapping(address account => bool) public forwarders;
    /// @notice Sum of `pending` amounts. `rescue` cannot touch it.
    uint256 public totalPending;

    event SourceSet(
        uint32 indexed domain, uint256 chainId, bytes32 adapter, bytes32 remoteToken, bool enabled
    );
    event ForwarderSet(address indexed account, bool allowed);
    event RemoteDepositReceived(
        bytes32 indexed key,
        uint32 indexed sourceDomain,
        uint256 indexed chainId,
        address depositor,
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
    error AlreadyPending(bytes32 key);
    error NotPending(bytes32 key);
    error NotForwarder();
    error PendingFunds(uint256 available);

    constructor(address bridge_, address messageTransmitter_, uint32 localDomain_) {
        if (bridge_ == address(0) || messageTransmitter_ == address(0)) revert ZeroAddress();
        bridge = IDepositBridge(bridge_);
        address token_ = bridge.usdc();
        if (token_ == address(0)) revert ZeroAddress();
        token = IERC20(token_);
        messageTransmitter = IMessageTransmitterV2(messageTransmitter_);
        localDomain = localDomain_;
        // Keeps the proven receipt to `Transfer` + `Deposit`: a per-forward
        // approve would add an `Approval` log. `bridge` is immutable and only
        // pulls from its own caller.
        if (!token.approve(bridge_, type(uint256).max)) revert TransferFailed();
    }

    /// @notice Trust burns from `adapter` of `remoteToken` on CCTP `domain`.
    ///         `chainId` is the EIP-155 id recorded in `RemoteDepositReceived`.
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

    /// @notice Allow `account` to call `forward`. It must send type 1 or 2
    ///         transactions.
    function setForwarder(address account, bool allowed) external onlyOwner {
        if (account == address(0)) revert ZeroAddress();
        forwarders[account] = allowed;
        emit ForwarderSet(account, allowed);
    }

    /// @notice `keccak256(abi.encode(sourceDomain, nonce))` — the `pending` key.
    function messageKey(uint32 sourceDomain, bytes32 nonce) public pure returns (bytes32) {
        return keccak256(abi.encode(sourceDomain, nonce));
    }

    /// @notice Verify `message` with Circle, mint, and park the deposit.
    ///         Permissionless: the attestation is the authorization, and the
    ///         message names this contract as `destinationCaller`, so no one
    ///         else can mint it out from under us.
    function receiveBurn(bytes calldata message, bytes calldata attestation)
        external
        nonReentrant
        returns (bytes32 key, uint256 minted)
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

        key = messageKey(parsed.sourceDomain, parsed.nonce);
        if (pending[key].amount != 0) revert AlreadyPending(key);

        uint256 beforeBal = token.balanceOf(address(this));
        if (!messageTransmitter.receiveMessage(message, attestation)) revert ReceiveFailed();
        uint256 afterBal = token.balanceOf(address(this));
        if (afterBal < beforeBal || afterBal - beforeBal != minted) {
            revert MintMismatch(minted, afterBal - beforeBal);
        }

        pending[key] = Pending({
            amount: minted, anAccount: anAccount, depositor: depositor, anWorkchain: anWorkchain
        });
        totalPending += minted;

        emit RemoteDepositReceived(
            key,
            parsed.sourceDomain,
            source.chainId,
            depositor,
            anAccount,
            minted,
            parsed.feeExecuted
        );
    }

    /// @notice Deposit a parked burn into the hub bridge. Emits no event of its
    ///         own, so the proven receipt is `Transfer` + `Deposit`.
    function forward(bytes32 key) external nonReentrant {
        Pending memory parked = pending[key];
        if (parked.amount == 0) revert NotPending(key);
        if (!forwarders[msg.sender] && msg.sender != parked.depositor) revert NotForwarder();

        delete pending[key];
        totalPending -= parked.amount;
        bridge.deposit(parked.amount, parked.anWorkchain, parked.anAccount);
    }

    /// @notice Pull tokens that were sent here outside `receiveBurn`. Parked
    ///         deposits are not available to it.
    function rescue(address token_, address to, uint256 amount) external onlyOwner nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        if (token_ == address(token)) {
            uint256 balance = token.balanceOf(address(this));
            uint256 available = balance > totalPending ? balance - totalPending : 0;
            if (amount > available) revert PendingFunds(available);
        }
        if (!IERC20(token_).transfer(to, amount)) revert TransferFailed();
        emit Rescued(token_, to, amount);
    }

    function _asBytes32(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}
