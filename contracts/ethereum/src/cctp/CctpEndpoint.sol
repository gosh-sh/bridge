// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../IERC20.sol";
import "./AnDestinationHook.sol";
import "./ITokenMessengerV2.sol";
import "./ReentrancyGuard.sol";
import "./TwoStepOwnable.sol";

/// @title CctpEndpoint
/// @notice One deployment per chain, many tokens.
///
///         Ingress (`depositToHub`): pull a local issuer token, burn it through
///         CCTP, and name the hub receiver as both the mint recipient and the
///         only `destinationCaller`. The hook carries the Acki Nacki account.
///         Which hub a token flies to is `setHub` — Arbitrum USDC and a later
///         EURC are two rows, not two contracts.
///
///         Egress (`bridgeOut`): the opposite direction, with no hook. A holder
///         who was paid on the hub (or who simply holds the token) burns it
///         toward a recipient on an allowlisted domain. Arbitrum is domain 3
///         once the owner enables it; the contract does not special-case it.
contract CctpEndpoint is TwoStepOwnable, ReentrancyGuard {
    struct HubRoute {
        uint32 destinationDomain;
        bytes32 hubReceiver;
        bool enabled;
    }

    ITokenMessengerV2 public immutable tokenMessenger;

    mapping(address token => HubRoute) public hubs;
    mapping(uint32 domain => bool) public egressDomains;

    event HubSet(
        address indexed token, uint32 destinationDomain, bytes32 hubReceiver, bool enabled
    );
    event EgressDomainSet(uint32 indexed domain, bool allowed);
    event HubDeposit(
        address indexed token,
        address indexed depositor,
        uint32 destinationDomain,
        bytes32 anAccount,
        uint256 amount,
        uint256 maxFee
    );
    event BridgedOut(
        address indexed token,
        address indexed sender,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        uint256 amount,
        uint256 maxFee
    );
    event Rescued(address indexed token, address indexed to, uint256 amount);

    error HubNotConfigured(address token);
    error EgressDisabled(uint32 domain);
    error ZeroAmount();
    error ZeroAccount();
    error ZeroRecipient();
    error FeeNotBelowAmount();
    error TransferFailed();
    error TransferAmountMismatch();

    constructor(address tokenMessenger_) {
        if (tokenMessenger_ == address(0)) revert ZeroAddress();
        tokenMessenger = ITokenMessengerV2(tokenMessenger_);
    }

    /// @notice Route `token` at a hub receiver on `destinationDomain`.
    ///         `hubReceiver` is that receiver's address on the destination,
    ///         left-padded to bytes32. Enabling requires a non-zero receiver.
    function setHub(address token, uint32 destinationDomain, bytes32 hubReceiver, bool enabled)
        external
        onlyOwner
    {
        if (token == address(0)) revert ZeroAddress();
        if (enabled && hubReceiver == bytes32(0)) revert ZeroRecipient();
        hubs[token] = HubRoute({
            destinationDomain: destinationDomain, hubReceiver: hubReceiver, enabled: enabled
        });
        emit HubSet(token, destinationDomain, hubReceiver, enabled);
    }

    /// @notice Allow or revoke `bridgeOut` toward a CCTP domain.
    function setEgressDomain(uint32 domain, bool allowed) external onlyOwner {
        egressDomains[domain] = allowed;
        emit EgressDomainSet(domain, allowed);
    }

    /// @param maxFee            Circle's fast-transfer ceiling, in token units.
    ///                          Must be strictly below `amount`. Zero selects a
    ///                          transfer that can be attested at finality with
    ///                          no fee.
    /// @param minFinalityThreshold `CctpDomains.FINALIZED` (2000) or `FAST` (1000).
    function depositToHub(
        address token,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 maxFee,
        uint32 minFinalityThreshold
    ) external nonReentrant {
        HubRoute memory route = hubs[token];
        if (!route.enabled) revert HubNotConfigured(token);
        if (amount == 0) revert ZeroAmount();
        if (anAccount == bytes32(0)) revert ZeroAccount();
        if (maxFee >= amount) revert FeeNotBelowAmount();

        _pull(token, amount);
        bytes memory hook = AnDestinationHook.encode(anWorkchain, anAccount, msg.sender);
        _approve(token, address(tokenMessenger), amount);
        tokenMessenger.depositForBurnWithHook(
            amount,
            route.destinationDomain,
            route.hubReceiver,
            token,
            route.hubReceiver,
            maxFee,
            minFinalityThreshold,
            hook
        );
        _approve(token, address(tokenMessenger), 0);

        emit HubDeposit(token, msg.sender, route.destinationDomain, anAccount, amount, maxFee);
    }

    /// @notice Burn `token` for minting on `destinationDomain`. No Acki Nacki
    ///         hook: `mintRecipient` receives the tokens directly.
    function bridgeOut(
        address token,
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        uint256 maxFee,
        uint32 minFinalityThreshold
    ) external nonReentrant {
        if (!egressDomains[destinationDomain]) {
            revert EgressDisabled(destinationDomain);
        }
        if (amount == 0) revert ZeroAmount();
        if (mintRecipient == bytes32(0)) revert ZeroRecipient();
        if (maxFee >= amount) revert FeeNotBelowAmount();

        _pull(token, amount);
        _approve(token, address(tokenMessenger), amount);
        tokenMessenger.depositForBurn(
            amount,
            destinationDomain,
            mintRecipient,
            token,
            bytes32(0),
            maxFee,
            minFinalityThreshold
        );
        _approve(token, address(tokenMessenger), 0);

        emit BridgedOut(token, msg.sender, destinationDomain, mintRecipient, amount, maxFee);
    }

    /// @notice Pull tokens that were sent here by mistake. A `depositToHub` or
    ///         `bridgeOut` that reverts leaves nothing behind: the pull and the
    ///         burn share one transaction.
    function rescue(address token, address to, uint256 amount) external onlyOwner nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        if (!IERC20(token).transfer(to, amount)) revert TransferFailed();
        emit Rescued(token, to, amount);
    }

    function _pull(address token, uint256 amount) internal {
        uint256 beforeBal = IERC20(token).balanceOf(address(this));
        if (!IERC20(token).transferFrom(msg.sender, address(this), amount)) {
            revert TransferFailed();
        }
        uint256 afterBal = IERC20(token).balanceOf(address(this));
        if (afterBal < beforeBal || afterBal - beforeBal != amount) {
            revert TransferAmountMismatch();
        }
    }

    /// @dev Approve zero before a non-zero value. Circle's USDC rejects a
    ///      non-zero to non-zero allowance change.
    function _approve(address token, address spender, uint256 amount) internal {
        IERC20 erc = IERC20(token);
        if (amount != 0 && erc.allowance(address(this), spender) != 0) {
            if (!erc.approve(spender, 0)) revert TransferFailed();
        }
        if (!erc.approve(spender, amount)) revert TransferFailed();
    }
}
