// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./NacklToken.sol";

library NacklMint {
    uint256 internal constant DELAY = 24 hours;

    uint256 internal constant WINDOW_HOURS = 25;

    struct Payout {
        address recipient;
        uint64 readyAt;
        bool executed;
        uint256 amount;
    }

    struct State {
        NacklToken token;
        uint256 dailyLimit;
        uint256 delayThreshold;
        mapping(uint256 => uint256) mintedInHour;
        mapping(uint256 => Payout) payouts;
        uint256 payoutCount;
        uint256 accepted;
        uint256 minted;
        uint256 pendingCount;
        bool handedOver;
        uint256[] windowQueue;
        uint256 windowHead;
        uint256[] delayQueue;
        uint256 delayHead;
    }

    event NacklListed(address indexed token, uint256 dailyLimit, uint256 delayThreshold);
    event NacklLimitsSet(uint256 dailyLimit, uint256 delayThreshold);
    event NacklPayoutQueued(
        uint256 indexed payoutId, address indexed recipient, uint256 amount, uint64 readyAt
    );
    event NacklPayoutExecuted(uint256 indexed payoutId, address indexed recipient, uint256 amount);
    event NacklMinterHandedOver(address indexed successor);

    error NacklNotMinter(address token);
    error NacklBadDecimals(uint8 decimals);
    error NacklLimitBelowWithdrawalCap(uint256 dailyLimit, uint256 maxWithdrawal);
    error NacklPayoutUnknown(uint256 payoutId);
    error NacklPayoutAlreadyExecuted(uint256 payoutId);
    error NacklPayoutNotDue(uint256 payoutId, uint64 readyAt);
    error NacklPayoutNotNext(uint256 payoutId, uint256 next);
    error NacklWindowFull(uint256 used, uint256 amount, uint256 dailyLimit);
    error NacklMinterGone();
    error NacklPayoutsPending(uint256 count);
    error NacklSuccessorNotReady(address successor);

    function list(
        State storage s,
        NacklToken token,
        uint256 dailyLimit,
        uint256 delayThreshold,
        uint256 maxWithdrawal,
        address predecessor
    ) public {
        address minter = token.owner();
        if (minter != address(this) && (minter != predecessor || predecessor == address(0))) {
            revert NacklNotMinter(address(token));
        }
        uint8 d = token.decimals();
        if (d != 9) revert NacklBadDecimals(d);
        s.token = token;
        _setLimits(s, dailyLimit, delayThreshold, maxWithdrawal);
        emit NacklListed(address(token), dailyLimit, delayThreshold);
    }

    function setLimits(
        State storage s,
        uint256 dailyLimit,
        uint256 delayThreshold,
        uint256 maxWithdrawal
    ) public {
        _setLimits(s, dailyLimit, delayThreshold, maxWithdrawal);
        emit NacklLimitsSet(dailyLimit, delayThreshold);
    }

    function pay(State storage s, address recipient, uint256 amount)
        public
        returns (uint256 payoutId)
    {
        if (s.handedOver) revert NacklMinterGone();
        s.accepted += amount;
        bool large = amount > s.delayThreshold;
        if (
            !large && next(s) == 0 && windowUsed(s) + amount <= s.dailyLimit
                && s.token.owner() == address(this)
        ) {
            _mint(s, recipient, amount);
            return 0;
        }
        payoutId = ++s.payoutCount;
        uint64 readyAt = uint64(block.timestamp + (large ? DELAY : 0));
        s.payouts[payoutId] =
            Payout({ recipient: recipient, readyAt: readyAt, executed: false, amount: amount });
        s.pendingCount += 1;
        if (large) s.delayQueue.push(payoutId);
        else s.windowQueue.push(payoutId);
        emit NacklPayoutQueued(payoutId, recipient, amount, readyAt);
    }

    function execute(State storage s, uint256 payoutId) public {
        if (s.handedOver) revert NacklMinterGone();
        Payout storage p = s.payouts[payoutId];
        if (p.recipient == address(0)) revert NacklPayoutUnknown(payoutId);
        if (p.executed) revert NacklPayoutAlreadyExecuted(payoutId);
        if (block.timestamp < p.readyAt) revert NacklPayoutNotDue(payoutId, p.readyAt);
        uint256 first = next(s);
        if (payoutId != first) revert NacklPayoutNotNext(payoutId, first);
        uint256 used = windowUsed(s);
        if (used + p.amount > s.dailyLimit) revert NacklWindowFull(used, p.amount, s.dailyLimit);
        if (s.token.owner() != address(this)) revert NacklNotMinter(address(s.token));
        p.executed = true;
        s.pendingCount -= 1;
        if (s.windowHead < s.windowQueue.length && s.windowQueue[s.windowHead] == payoutId) {
            s.windowHead += 1;
        } else {
            s.delayHead += 1;
        }
        _mint(s, p.recipient, p.amount);
        emit NacklPayoutExecuted(payoutId, p.recipient, p.amount);
    }

    function handOver(State storage s, address successor) public {
        if (s.handedOver) revert NacklMinterGone();
        if (s.pendingCount != 0) revert NacklPayoutsPending(s.pendingCount);
        (bool ok, bytes memory ret) =
            successor.staticcall(abi.encodeWithSignature("tokenInfo(uint32)", uint32(1)));
        if (!ok || ret.length < 32 || abi.decode(ret, (address)) != address(s.token)) {
            revert NacklSuccessorNotReady(successor);
        }
        s.handedOver = true;
        s.token.transferOwnership(successor);
        emit NacklMinterHandedOver(successor);
    }

    function next(State storage s) public view returns (uint256) {
        uint256 w = s.windowHead < s.windowQueue.length ? s.windowQueue[s.windowHead] : 0;
        uint256 d = s.delayHead < s.delayQueue.length ? s.delayQueue[s.delayHead] : 0;
        if (d != 0 && s.payouts[d].readyAt > block.timestamp) d = 0;
        if (w == 0) return d;
        if (d == 0) return w;
        Payout storage pw = s.payouts[w];
        Payout storage pd = s.payouts[d];
        if (pd.readyAt < pw.readyAt || (pd.readyAt == pw.readyAt && d < w)) return d;
        return w;
    }

    function windowUsed(State storage s) public view returns (uint256 used) {
        uint256 hour = block.timestamp / 1 hours;
        uint256 first = hour >= WINDOW_HOURS - 1 ? hour - (WINDOW_HOURS - 1) : 0;
        for (uint256 h = first; h <= hour; h++) {
            used += s.mintedInHour[h];
        }
    }

    function _setLimits(
        State storage s,
        uint256 dailyLimit,
        uint256 delayThreshold,
        uint256 maxWithdrawal
    ) private {
        if (dailyLimit < maxWithdrawal) {
            revert NacklLimitBelowWithdrawalCap(dailyLimit, maxWithdrawal);
        }
        s.dailyLimit = dailyLimit;
        s.delayThreshold = delayThreshold;
    }

    function _mint(State storage s, address recipient, uint256 amount) private {
        s.mintedInHour[block.timestamp / 1 hours] += amount;
        s.minted += amount;
        s.token.mint(recipient, amount);
    }
}
