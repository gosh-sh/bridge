//! What the Acki Nacki bridge needs to finalize a deposit from its receipt.
//!
//! `finalizeDeposit(chainId, header, txIndex, receiptProof, logIndex)`
//! proves the receipt of transaction `txIndex` against the block's
//! `receiptsRoot` and reads log `logIndex` of that receipt (counted within
//! the receipt). Both indices are `uint16`. The receipt must be a success,
//! and the log the bridge's `Deposit` in its one shape: three topics and
//! four data words. The header and the proof nodes travel in one external
//! message, and the block manager's `/v2/messages` takes at most 64 KiB
//! for the whole of it.

/// The most header and proof-node bytes a `finalizeDeposit` may carry
/// ([`deposit_receipt::DepositWitness::payload_len`]). The block manager's
/// `/v2/messages` takes 64 KiB for the whole message; the rest is left to
/// the message's own header, the ABI encoding and the cell tree.
pub const MAX_FINALIZE_PAYLOAD: usize = 48 * 1024;

/// Why the bridge cannot finalize a deposit from its receipt. Final: the
/// same receipt gives the same answer every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfinalizable {
    /// The transaction failed.
    Failed,
    /// The log is not the bridge's `Deposit` in the shape the bridge reads.
    NotDepositLog {
        /// What is wrong with it.
        reason: String,
    },
    /// The transaction's position in its block, or the log's position in
    /// its receipt, does not fit the bridge's `uint16`.
    IndexTooLarge {
        /// `txIndex` or `logIndex`.
        what: &'static str,
        /// The index found.
        value: u64,
    },
    /// The header and the receipt proof are too large for one message.
    PayloadTooLarge {
        /// Their length in bytes.
        len: usize,
    },
}

impl std::fmt::Display for Unfinalizable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed => write!(f, "the transaction failed"),
            Self::NotDepositLog {
                reason,
            } => write!(f, "the bridge does not read its log as a deposit: {reason}"),
            Self::IndexTooLarge {
                what,
                value,
            } => write!(
                f,
                "its {what} is {value}; the bridge takes at most {}",
                u16::MAX
            ),
            Self::PayloadTooLarge {
                len,
            } => write!(
                f,
                "the block header and the receipt proof are {len} bytes; one finalizeDeposit \
                 message carries at most {MAX_FINALIZE_PAYLOAD}"
            ),
        }
    }
}

/// Refuses a transaction index or a receipt-local log index that does not
/// fit the bridge's `uint16`.
pub fn check_indices(tx_index: u64, receipt_log_index: u64) -> Result<(), Unfinalizable> {
    for (what, value) in [("txIndex", tx_index), ("logIndex", receipt_log_index)] {
        if value > u64::from(u16::MAX) {
            return Err(Unfinalizable::IndexTooLarge {
                what,
                value,
            });
        }
    }
    Ok(())
}

/// Refuses a header and receipt proof of `len` bytes that do not fit one
/// message.
pub fn check_payload(len: usize) -> Result<(), Unfinalizable> {
    if len > MAX_FINALIZE_PAYLOAD {
        return Err(Unfinalizable::PayloadTooLarge {
            len,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_up_to_u16_max_fit() {
        assert_eq!(check_indices(65_535, 65_535), Ok(()));
        assert_eq!(
            check_indices(65_536, 0),
            Err(Unfinalizable::IndexTooLarge {
                what: "txIndex",
                value: 65_536
            })
        );
        assert_eq!(
            check_indices(0, 65_536),
            Err(Unfinalizable::IndexTooLarge {
                what: "logIndex",
                value: 65_536
            })
        );
    }

    #[test]
    fn the_payload_bound_is_inclusive_and_below_the_message_limit() {
        assert_eq!(check_payload(MAX_FINALIZE_PAYLOAD), Ok(()));
        assert!(check_payload(MAX_FINALIZE_PAYLOAD + 1).is_err());
        const { assert!(MAX_FINALIZE_PAYLOAD < 64 * 1024) };
    }
}
