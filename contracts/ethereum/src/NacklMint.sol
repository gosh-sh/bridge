// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./NacklToken.sol";

library NacklMint {
    uint256 internal constant WINDOW_HOURS = 25;

    struct State {
        NacklToken token;
        uint256 dailyLimit;
        mapping(uint256 => uint256) mintedInHour;
        uint256 minted;
        bool handedOver;
    }

    event NacklListed(address indexed token, uint256 dailyLimit);
    event NacklLimitsSet(uint256 dailyLimit);
    event NacklMinterHandedOver(address indexed successor);

    error NacklNotMinter(address token);
    error NacklBadDecimals(uint8 decimals);
    error NacklLimitBelowWithdrawalCap(uint256 dailyLimit, uint256 maxWithdrawal);
    error NacklWindowFull(uint256 used, uint256 amount, uint256 dailyLimit);
    error NacklMinterGone();
    error NacklSuccessorNotReady(address successor);

    function list(
        State storage s,
        NacklToken token,
        uint256 dailyLimit,
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
        _setLimit(s, dailyLimit, maxWithdrawal);
        emit NacklListed(address(token), dailyLimit);
    }

    function setLimit(State storage s, uint256 dailyLimit, uint256 maxWithdrawal) public {
        _setLimit(s, dailyLimit, maxWithdrawal);
        emit NacklLimitsSet(dailyLimit);
    }

    function pay(State storage s, address recipient, uint256 amount) public {
        if (s.handedOver) revert NacklMinterGone();
        if (s.token.owner() != address(this)) revert NacklNotMinter(address(s.token));
        uint256 used = windowUsed(s);
        if (used + amount > s.dailyLimit) revert NacklWindowFull(used, amount, s.dailyLimit);
        s.mintedInHour[block.timestamp / 1 hours] += amount;
        s.minted += amount;
        s.token.mint(recipient, amount);
    }

    function handOver(State storage s, address successor) public {
        if (s.handedOver) revert NacklMinterGone();
        (bool ok, bytes memory ret) =
            successor.staticcall(abi.encodeWithSignature("tokenInfo(uint32)", uint32(1)));
        if (!ok || ret.length < 32 || abi.decode(ret, (address)) != address(s.token)) {
            revert NacklSuccessorNotReady(successor);
        }
        s.handedOver = true;
        s.token.transferOwnership(successor);
        emit NacklMinterHandedOver(successor);
    }

    function windowUsed(State storage s) public view returns (uint256 used) {
        uint256 hour = block.timestamp / 1 hours;
        uint256 first = hour >= WINDOW_HOURS - 1 ? hour - (WINDOW_HOURS - 1) : 0;
        for (uint256 h = first; h <= hour; h++) {
            used += s.mintedInHour[h];
        }
    }

    function _setLimit(State storage s, uint256 dailyLimit, uint256 maxWithdrawal) private {
        if (dailyLimit < maxWithdrawal) {
            revert NacklLimitBelowWithdrawalCap(dailyLimit, maxWithdrawal);
        }
        s.dailyLimit = dailyLimit;
    }
}
