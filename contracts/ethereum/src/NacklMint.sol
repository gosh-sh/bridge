// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import { NacklToken } from "./NacklToken.sol";

/// @dev What `NacklMint.migrate` reads from the successor bridge.
interface INacklSuccessor {
    function tokenInfo(uint32 tokenId)
        external
        view
        returns (
            address token,
            uint8 decimals,
            bool depositsEnabled,
            uint256 maxTransfer,
            uint256 tvlCap,
            uint256 liabilities
        );
    function maxWithdrawalAmount(uint32 tokenId) external view returns (uint256);
}

/// @title NACKL mint and burn for `AckiNackiBridge`
/// @notice The bridge owns ERC20 NACKL and mints it for proven withdrawals,
///         which lock NACKL on Acki Nacki, and burns it on deposit. A forged
///         proof is bounded here instead of by custody: the bridge mints at
///         most `dailyLimit` within any 24 hours, and a payout above
///         `delayThreshold` waits 24 hours, long enough to pause the bridge.
///         A payout that cannot be minted yet is queued under its nullifier;
///         anyone executes it once it is due and fits. It cannot be cancelled.
/// @dev An external library, linked into the bridge, so that the bridge stays
///      within EIP-170. Every function runs on the bridge's storage.
library NacklMint {
    uint32 internal constant TOKEN_ID = 1;
    uint8 internal constant DECIMALS = 9;
    uint256 internal constant DELAY = 24 hours;
    /// @dev A mint counts in its hour's slot until the 25th hour after it, so
    ///      for more than 24 hours and at most 25: any 24-hour interval sees
    ///      every mint in it.
    uint256 internal constant WINDOW_HOURS = 25;

    struct Payout {
        address recipient;
        uint64 readyAt;
        uint256 amount;
    }

    struct State {
        NacklToken token;
        uint256 dailyLimit;
        uint256 delayThreshold;
        /// @dev Set by `migrate`; this bridge mints no more once it is.
        address migratedTo;
        uint256 queuedCount;
        /// @dev Slot `hour % WINDOW_HOURS` packs `hour << 192 | minted`.
        uint256[WINDOW_HOURS] window;
        mapping(bytes32 => Payout) queue;
    }

    event NacklMintLimitsSet(uint256 dailyLimit, uint256 delayThreshold);
    event NacklPayoutQueued(
        uint256 indexed nullifier, address indexed recipient, uint256 amount, uint256 readyAt
    );
    event NacklPayoutAbandoned(uint256 indexed nullifier, address indexed recipient, uint256 amount);
    event NacklMinterMigrated(address indexed successor);

    error NacklAlreadyListed();
    error NacklDecimalsMismatch(uint8 decimals);
    error NacklLimitsInvalid(uint256 dailyLimit, uint256 maxWithdrawal);
    error NacklPayoutNotQueued(uint256 nullifier);
    error NacklPayoutNotDue(uint256 nullifier, uint256 readyAt);
    error NacklMintLimitReached(uint256 amount, uint256 capacity);
    error NacklQueueNotEmpty(uint256 queued);
    error NacklMinterHandedOver(address successor);
    error NacklSuccessorMismatch(address successor);
    error NacklNotCustodied();
    error NacklNotListed();

    /// @notice Record ERC20 NACKL and its limits; the bridge lists it under id 1.
    function list(
        State storage s,
        address token,
        uint256 maxTransfer,
        uint256 dailyLimit,
        uint256 delayThreshold
    ) external {
        if (address(s.token) != address(0)) revert NacklAlreadyListed();
        uint8 d = NacklToken(token).decimals();
        if (d != DECIMALS) revert NacklDecimalsMismatch(d);
        s.token = NacklToken(token);
        _setLimits(s, dailyLimit, delayThreshold, maxTransfer);
    }

    function setLimits(
        State storage s,
        uint256 dailyLimit,
        uint256 delayThreshold,
        uint256 maxWithdrawal
    ) external {
        _setLimits(s, dailyLimit, delayThreshold, maxWithdrawal);
    }

    /// @notice Burn `amount` of the caller's ERC20 NACKL for a deposit; the
    ///         caller approves the bridge first, as for any token.
    function burn(State storage s, address from, uint256 amount) external {
        s.token.burnFrom(from, amount);
    }

    /// @notice Mint a proven payout now, or queue it.
    /// @return minted Whether it was minted in this call.
    function payout(State storage s, uint256 nullifier, address recipient, uint256 amount)
        external
        returns (bool minted)
    {
        _requireMinter(s);
        if (amount > s.delayThreshold) {
            _enqueue(s, nullifier, recipient, amount, block.timestamp + DELAY);
            return false;
        }
        if (amount > _capacity(s)) {
            _enqueue(s, nullifier, recipient, amount, block.timestamp);
            return false;
        }
        _mint(s, recipient, amount);
        return true;
    }

    /// @notice Mint a queued payout that is due and fits.
    function execute(State storage s, uint256 nullifier)
        external
        returns (address recipient, uint256 amount)
    {
        _requireMinter(s);
        Payout memory p = s.queue[bytes32(nullifier)];
        if (p.recipient == address(0)) revert NacklPayoutNotQueued(nullifier);
        if (block.timestamp < p.readyAt) revert NacklPayoutNotDue(nullifier, p.readyAt);
        uint256 room = _capacity(s);
        if (p.amount > room) revert NacklMintLimitReached(p.amount, room);
        delete s.queue[bytes32(nullifier)];
        s.queuedCount -= 1;
        _mint(s, p.recipient, p.amount);
        return (p.recipient, p.amount);
    }

    /// @notice Hand ERC20 NACKL to `successor`. The caller has checked that
    ///         this bridge is paused and that `successor` names it as its
    ///         predecessor. `abandoned` lists the queued payouts the owner
    ///         holds to be forged; every other one must have been executed.
    function migrate(
        State storage s,
        address successor,
        uint256[] calldata abandoned,
        uint256 maxWithdrawal
    ) external {
        _requireMinter(s);
        NacklToken token = s.token;
        if (address(token) == address(0)) revert NacklNotListed();
        (address listed,,,,,) = INacklSuccessor(successor).tokenInfo(TOKEN_ID);
        if (
            listed != address(token)
                || INacklSuccessor(successor).maxWithdrawalAmount(TOKEN_ID) < maxWithdrawal
        ) {
            revert NacklSuccessorMismatch(successor);
        }
        for (uint256 i = 0; i < abandoned.length; ++i) {
            Payout memory p = s.queue[bytes32(abandoned[i])];
            if (p.recipient == address(0)) revert NacklPayoutNotQueued(abandoned[i]);
            delete s.queue[bytes32(abandoned[i])];
            s.queuedCount -= 1;
            emit NacklPayoutAbandoned(abandoned[i], p.recipient, p.amount);
        }
        if (s.queuedCount != 0) revert NacklQueueNotEmpty(s.queuedCount);
        s.migratedTo = successor;
        token.transferOwnership(successor);
        if (token.owner() != successor) revert NacklSuccessorMismatch(successor);
        emit NacklMinterMigrated(successor);
    }

    /// @notice What the bridge can still mint before the window frees more.
    function capacity(State storage s) external view returns (uint256) {
        return _capacity(s);
    }

    function _setLimits(
        State storage s,
        uint256 dailyLimit,
        uint256 delayThreshold,
        uint256 maxWithdrawal
    ) private {
        // The window packs an hour's mints into 192 bits.
        if (dailyLimit == 0 || dailyLimit < maxWithdrawal || dailyLimit > type(uint128).max) {
            revert NacklLimitsInvalid(dailyLimit, maxWithdrawal);
        }
        s.dailyLimit = dailyLimit;
        s.delayThreshold = delayThreshold;
        emit NacklMintLimitsSet(dailyLimit, delayThreshold);
    }

    function _requireMinter(State storage s) private view {
        if (s.migratedTo != address(0)) revert NacklMinterHandedOver(s.migratedTo);
    }

    function _enqueue(
        State storage s,
        uint256 nullifier,
        address recipient,
        uint256 amount,
        uint256 readyAt
    ) private {
        s.queue[bytes32(nullifier)] =
            Payout({ recipient: recipient, readyAt: uint64(readyAt), amount: amount });
        s.queuedCount += 1;
        emit NacklPayoutQueued(nullifier, recipient, amount, readyAt);
    }

    function _mint(State storage s, address recipient, uint256 amount) private {
        uint256 hour = block.timestamp / 1 hours;
        uint256 slot = hour % WINDOW_HOURS;
        uint256 packed = s.window[slot];
        uint256 minted = packed >> 192 == hour ? uint192(packed) : 0;
        s.window[slot] = hour << 192 | (minted + amount);
        s.token.mint(recipient, amount);
    }

    function _capacity(State storage s) private view returns (uint256) {
        uint256 hour = block.timestamp / 1 hours;
        uint256 used;
        for (uint256 i = 0; i < WINDOW_HOURS; ++i) {
            uint256 packed = s.window[i];
            if (hour - (packed >> 192) < WINDOW_HOURS) used += uint192(packed);
        }
        uint256 limit = s.dailyLimit;
        return used < limit ? limit - used : 0;
    }
}
