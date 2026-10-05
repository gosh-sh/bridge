//! What the deposit circuit can prove, copied from its source.
//!
//! The circuit proves only an EIP-1559 transaction with calldata of at
//! most 256 bytes and an access list whose RLP is at most 64 bytes, and
//! its receipt parser reads at most 20 logs with at most 256 bytes of data
//! each. `limits_match_the_circuit_source` holds these numbers to the
//! circuit. The circuit does not constrain `to`; requiring the bridge
//! there is this CLI's own rule, so a call routed through another
//! contract is caught here rather than after the proof.

use alloy_primitives::Address;

/// The only transaction type the circuit proves.
pub const EIP1559_TX_TYPE: u8 = 2;
/// The most calldata bytes the circuit reads.
pub const MAX_TX_CALLDATA_BYTE_LEN: usize = 256;
/// Compared with the full RLP encoding, list header included. Where the
/// circuit counts the payload only, this is one byte stricter.
pub const MAX_TX_ACCESS_LIST_LEN: usize = 64;
/// The most receipt logs the prover parses.
pub const MAX_RECEIPT_LOGS: usize = 20;
/// The most data bytes per log the prover parses.
pub const MAX_LOG_DATA_BYTE_LEN: usize = 256;
/// The circuit degree the CLI passes as `--degree`;
/// `configs/circuit_params.json` must carry the same `k`.
pub const PROVER_DEGREE: u32 = 18;

/// The parts of a transaction the circuit's limits are about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxShape {
    /// The EIP-2718 transaction type.
    pub tx_type: u8,
    /// The recipient; `None` for a contract creation.
    pub to: Option<Address>,
    /// Calldata length in bytes.
    pub input_len: usize,
    /// Length of the RLP encoding of the access list, header included.
    pub access_list_rlp_len: usize,
}

/// Why a transaction or receipt cannot be proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeViolation {
    /// The transaction is not type 2.
    NotEip1559 {
        /// The type found.
        tx_type: u8,
    },
    /// Calldata exceeds the circuit's bound.
    CalldataTooLong {
        /// The calldata length found.
        len: usize,
    },
    /// The access list exceeds the circuit's bound.
    AccessListTooLong {
        /// The RLP length found.
        len: usize,
    },
    /// The transaction was not sent straight to the bridge.
    NotToBridge {
        /// The recipient found.
        to: Option<Address>,
    },
    /// The receipt has more logs than the prover parses.
    TooManyLogs {
        /// The log count found.
        count: usize,
    },
    /// The `Deposit` log data exceeds the prover's bound.
    LogDataTooLong {
        /// The data length found.
        len: usize,
    },
}

impl std::fmt::Display for ShapeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEip1559 {
                tx_type,
            } => write!(
                f,
                "the transaction is type {tx_type}; the deposit circuit proves only type 2 \
                 (EIP-1559)"
            ),
            Self::CalldataTooLong {
                len,
            } => write!(
                f,
                "the transaction carries {len} bytes of calldata; the circuit reads at most \
                 {MAX_TX_CALLDATA_BYTE_LEN}"
            ),
            Self::AccessListTooLong {
                len,
            } => write!(
                f,
                "the access list encodes to {len} bytes; the circuit reads at most \
                 {MAX_TX_ACCESS_LIST_LEN}"
            ),
            Self::NotToBridge {
                to,
            } => write!(
                f,
                "the transaction was sent to {} rather than to the bridge, i.e. through another \
                 contract",
                to.map(|a| a.to_string())
                    .unwrap_or_else(|| "no address (contract creation)".into())
            ),
            Self::TooManyLogs {
                count,
            } => write!(
                f,
                "the receipt has {count} logs; the prover reads at most {MAX_RECEIPT_LOGS}"
            ),
            Self::LogDataTooLong {
                len,
            } => write!(
                f,
                "the Deposit log carries {len} bytes of data; the prover reads at most \
                 {MAX_LOG_DATA_BYTE_LEN}"
            ),
        }
    }
}

/// Checks a transaction against the circuit's limits and the CLI's own
/// rule that it goes straight to `bridge`.
pub fn check_tx_shape(shape: &TxShape, bridge: Address) -> Result<(), ShapeViolation> {
    if shape.tx_type != EIP1559_TX_TYPE {
        return Err(ShapeViolation::NotEip1559 {
            tx_type: shape.tx_type,
        });
    }
    if shape.input_len > MAX_TX_CALLDATA_BYTE_LEN {
        return Err(ShapeViolation::CalldataTooLong {
            len: shape.input_len,
        });
    }
    if shape.access_list_rlp_len > MAX_TX_ACCESS_LIST_LEN {
        return Err(ShapeViolation::AccessListTooLong {
            len: shape.access_list_rlp_len,
        });
    }
    if shape.to != Some(bridge) {
        return Err(ShapeViolation::NotToBridge {
            to: shape.to,
        });
    }
    Ok(())
}

/// Checks a receipt against the prover parser's bounds.
pub fn check_receipt_bounds(
    log_count: usize,
    deposit_log_data_len: usize,
) -> Result<(), ShapeViolation> {
    if log_count > MAX_RECEIPT_LOGS {
        return Err(ShapeViolation::TooManyLogs {
            count: log_count,
        });
    }
    if deposit_log_data_len > MAX_LOG_DATA_BYTE_LEN {
        return Err(ShapeViolation::LogDataTooLong {
            len: deposit_log_data_len,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");

    fn shape(tx_type: u8, input_len: usize, al: usize) -> TxShape {
        TxShape {
            tx_type,
            to: Some(BRIDGE),
            input_len,
            access_list_rlp_len: al,
        }
    }

    #[test]
    fn only_a_type_2_transaction_is_provable() {
        for ty in [0u8, 1, 4] {
            assert_eq!(
                check_tx_shape(&shape(ty, 100, 1), BRIDGE),
                Err(ShapeViolation::NotEip1559 {
                    tx_type: ty
                })
            );
        }
        assert_eq!(check_tx_shape(&shape(2, 100, 1), BRIDGE), Ok(()));
    }

    #[test]
    fn calldata_and_access_list_bounds_are_inclusive() {
        assert_eq!(check_tx_shape(&shape(2, 256, 64), BRIDGE), Ok(()));
        assert_eq!(
            check_tx_shape(&shape(2, 257, 1), BRIDGE),
            Err(ShapeViolation::CalldataTooLong {
                len: 257
            })
        );
        assert_eq!(
            check_tx_shape(&shape(2, 100, 65), BRIDGE),
            Err(ShapeViolation::AccessListTooLong {
                len: 65
            })
        );
    }

    #[test]
    fn a_call_through_another_contract_is_refused_by_the_cli() {
        let mut s = shape(2, 100, 1);
        s.to = Some(address!("5ff137d4b0fdcd49dca30c7cf57e578a026d2789"));
        assert!(matches!(
            check_tx_shape(&s, BRIDGE),
            Err(ShapeViolation::NotToBridge { .. })
        ));
    }

    #[test]
    fn receipt_bounds_match_the_prover_parser() {
        assert_eq!(check_receipt_bounds(20, 256), Ok(()));
        assert_eq!(
            check_receipt_bounds(21, 128),
            Err(ShapeViolation::TooManyLogs {
                count: 21
            })
        );
        assert_eq!(
            check_receipt_bounds(2, 257),
            Err(ShapeViolation::LogDataTooLong {
                len: 257
            })
        );
    }

    /// The limits are copied from the circuit, which lives in another
    /// cargo tree. This reads the circuit's source and fails when the
    /// numbers move there and not here.
    #[test]
    fn limits_match_the_circuit_source() {
        let circuit = include_str!("../../../../deposit-prover/src/circuit_v2.rs");
        let prover = include_str!("../../../../deposit-prover/examples/export_blake2b_proof.rs");
        let konst = |src: &str, name: &str| -> u64 {
            let needle = format!("pub const {name}: ");
            let line = src
                .lines()
                .find(|l| l.trim_start().starts_with(&needle))
                .unwrap_or_else(|| panic!("{name} is gone from the circuit source"));
            line.rsplit('=')
                .next()
                .unwrap()
                .trim()
                .trim_end_matches(';')
                .parse()
                .unwrap()
        };
        assert_eq!(
            konst(circuit, "MAX_TX_CALLDATA_BYTE_LEN"),
            MAX_TX_CALLDATA_BYTE_LEN as u64
        );
        assert_eq!(
            konst(circuit, "MAX_TX_ACCESS_LIST_LEN"),
            MAX_TX_ACCESS_LIST_LEN as u64
        );
        assert_eq!(konst(circuit, "EIP1559_TX_TYPE"), EIP1559_TX_TYPE as u64);
        // The prover's own defaults for the three bounds the CLI passes explicitly.
        for (flag, want) in [
            ("degree", PROVER_DEGREE as u64),
            ("max_data_byte_len", MAX_LOG_DATA_BYTE_LEN as u64),
            ("max_log_num", MAX_RECEIPT_LOGS as u64),
        ] {
            let at = prover
                .find(&format!("    {flag}: "))
                .unwrap_or_else(|| panic!("{flag}"));
            let attr = &prover[..at];
            let last = attr.rfind("default_value = \"").unwrap();
            let value = &attr[last + 17..];
            assert_eq!(
                &value[..value.find('"').unwrap()],
                want.to_string(),
                "{flag}"
            );
        }
    }
}
