//! What the Acki Nacki bridge needs to finalize a deposit from its receipt.
//!
//! `finalizeDeposit(chainId, header, txIndex, receiptProof, logIndex)`
//! proves the receipt of transaction `txIndex` against the block's
//! `receiptsRoot` and reads log `logIndex` of that receipt (counted within
//! the receipt). Both indices are `uint16`. The receipt must be a success,
//! and the log the bridge's `Deposit` in its one shape: three topics and
//! four data words. The bridge checks all of that before it accepts the
//! external message, so the check has to fit the gas such a message may
//! spend first ([`deposit_receipt::AN_GAS_CREDIT`]).

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
    /// Checking the receipt proof would take more gas than the bridge may
    /// spend before it accepts the message.
    OverGasCredit {
        /// The proof's size and the gas it may take.
        reason: String,
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
            Self::OverGasCredit {
                reason,
            } => write!(f, "the bridge cannot accept it: {reason}"),
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

/// Refuses a receipt proof the bridge could not check within the gas it
/// may spend before accepting the message.
pub fn check_gas(witness: &deposit_receipt::DepositWitness) -> Result<(), Unfinalizable> {
    witness
        .check_gas()
        .map_err(|reason| Unfinalizable::OverGasCredit {
            reason,
        })
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
    fn a_proof_over_the_gas_credit_is_refused() {
        let mut w = deposit_receipt::DepositWitness {
            chain_id: 1,
            block_number: 1,
            block_hash: Default::default(),
            header: vec![0; 600].into(),
            tx_index: 0,
            receipt_proof: vec![vec![0; 500].into(); 3],
            log_index: 60,
        };
        assert_eq!(check_gas(&w), Ok(()));
        w.log_index = 160;
        let e = check_gas(&w).unwrap_err();
        assert!(e.to_string().contains("the bridge cannot accept it"), "{e}");
    }
}
