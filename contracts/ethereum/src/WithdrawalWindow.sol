// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @notice Rolling 24-hour withdrawal limit for one token, kept in 25 hourly
///         buckets: the current hour and the 24 before it.
library WithdrawalWindow {
    uint256 internal constant WINDOW_HOURS = 25;

    struct Window {
        uint256 dailyLimit;
        mapping(uint256 => uint256) paidInHour;
    }

    /// @notice Paying `amount` would take the window of `tokenId` above
    ///         `dailyLimit`. The proof stays unspent and pays once the
    ///         window has room.
    error WithdrawalWindowFull(uint32 tokenId, uint256 used, uint256 amount, uint256 dailyLimit);

    /// @notice Count `amount` into the current hour, or revert
    ///         `WithdrawalWindowFull`.
    function consume(Window storage w, uint32 tokenId, uint256 amount) public {
        uint256 used_ = used(w);
        uint256 limit = w.dailyLimit;
        if (used_ > limit || amount > limit - used_) {
            revert WithdrawalWindowFull(tokenId, used_, amount, limit);
        }
        w.paidInHour[block.timestamp / 1 hours] += amount;
    }

    /// @notice Paid out in the current hour and the 24 before it.
    function used(Window storage w) public view returns (uint256 total) {
        uint256 hour = block.timestamp / 1 hours;
        uint256 first = hour >= WINDOW_HOURS - 1 ? hour - (WINDOW_HOURS - 1) : 0;
        for (uint256 h = first; h <= hour; h++) {
            total += w.paidInHour[h];
        }
    }
}
