// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @notice Rolling 24-hour withdrawal limit for one token, kept in 25 hourly
///         buckets: the current hour and the 24 before it.
library WithdrawalWindow {
    uint256 internal constant WINDOW_HOURS = 25;

    struct Bucket {
        uint256 hour;
        uint256 paid;
    }

    struct Window {
        uint256 dailyLimit;
        Bucket[WINDOW_HOURS] buckets;
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
        uint256 hour = block.timestamp / 1 hours;
        Bucket storage b = w.buckets[hour % WINDOW_HOURS];
        if (b.hour != hour) {
            b.hour = hour;
            b.paid = amount;
        } else {
            b.paid += amount;
        }
    }

    /// @notice Paid out in the current hour and the 24 before it.
    function used(Window storage w) public view returns (uint256 total) {
        uint256 hour = block.timestamp / 1 hours;
        for (uint256 i = 0; i < WINDOW_HOURS; i++) {
            Bucket storage b = w.buckets[i];
            if (b.hour <= hour && b.hour + WINDOW_HOURS > hour) {
                total += b.paid;
            }
        }
    }
}
