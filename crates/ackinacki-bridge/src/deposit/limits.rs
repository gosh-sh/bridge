//! What the deposit circuit can prove, copied from its source.
//!
//! The circuit proves an EIP-2930 or EIP-1559 transaction with calldata of
//! at most 2048 bytes. Type 2 access-list payload is at most 512 bytes;
//! type 1 access list sits in the same field slot as type 2 calldata, so
//! it may be 2048 bytes. The receipt parser reads at most 20 logs; each
//! log's payload is at most 2211 bytes (2048 of data with four
//! topics; more data if there are fewer topics).
//! `limits_match_the_circuit_source` holds these numbers to the circuit.
//! Fitting these bounds is necessary, not sufficient: the receipt must
//! also fit the prover's keccak budget (about six logs at 2048 bytes),
//! and a type-1 transaction's calldata and access list together must fit
//! its ~2805-byte leaf. The prover checks those before proving and exits
//! with [`PROVER_UNPROVABLE_EXIT`]. The circuit does not constrain `to`;
//! requiring the bridge there is this CLI's own rule, so a call routed
//! through another contract is caught here rather than after the proof.

use alloy_primitives::Address;

/// EIP-2930 type byte the circuit accepts.
pub const EIP2930_TX_TYPE: u8 = 1;
/// EIP-1559 type byte the circuit accepts.
pub const EIP1559_TX_TYPE: u8 = 2;
/// The most calldata bytes the circuit reads.
pub const MAX_TX_CALLDATA_BYTE_LEN: usize = 2048;
/// Type-2 access-list payload (RLP header excluded). Type 1 may use
/// [`MAX_TX_CALLDATA_BYTE_LEN`] at the merged field slot.
pub const MAX_TX_ACCESS_LIST_LEN: usize = 512;
/// The most receipt logs the prover parses.
pub const MAX_RECEIPT_LOGS: usize = 20;
/// The circuit config's `--max-data-byte-len`; also the data field of a
/// four-topic log at [`MAX_ENCODED_LOG_LEN`].
pub const MAX_LOG_DATA_BYTE_LEN: usize = 2048;
/// Topics axiom-eth allows on each receipt log.
pub const MAX_LOG_TOPICS: usize = 4;
/// Max payload of one receipt log, axiom-eth `calc_max_log_len`
/// (list body, header excluded).
pub const MAX_ENCODED_LOG_LEN: usize =
    3 + 21 + 3 + 33 * MAX_LOG_TOPICS + 3 + MAX_LOG_DATA_BYTE_LEN + 1;
/// The exit code of the prover tools for a deposit the circuit cannot
/// prove (`deposit-prover/src/provable.rs`). Final: the same deposit gives
/// the same answer every time.
pub const PROVER_UNPROVABLE_EXIT: i32 = 3;
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
    /// The transaction is not type 1 or 2.
    NotTypedTx {
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
    /// Some receipt log's payload exceeds the prover's bound.
    EncodedLogTooLong {
        /// The payload length found.
        len: usize,
    },
    /// The prover found it unprovable: the keccak budget, a type-1 leaf
    /// over its cap, or a bound only the witness shows.
    ProverRefused {
        /// The prover's own words.
        reason: String,
    },
}

impl std::fmt::Display for ShapeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTypedTx {
                tx_type,
            } => write!(
                f,
                "the transaction is type {tx_type}; the deposit circuit proves only type 1 \
                 (EIP-2930) or type 2 (EIP-1559)"
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
                "the access list payload is {len} bytes; the circuit reads at most \
                 {MAX_TX_ACCESS_LIST_LEN} for type 2 (2048 for type 1)"
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
            Self::EncodedLogTooLong {
                len,
            } => write!(
                f,
                "a receipt log payload is {len} bytes; the circuit reads at most \
                 {MAX_ENCODED_LOG_LEN}"
            ),
            Self::ProverRefused {
                reason,
            } => write!(f, "the prover refused it: {reason}"),
        }
    }
}

fn access_list_max(tx_type: u8) -> usize {
    if tx_type == EIP2930_TX_TYPE {
        MAX_TX_CALLDATA_BYTE_LEN
    } else {
        MAX_TX_ACCESS_LIST_LEN
    }
}

/// Payload of an RLP list given its encoded length, header excluded.
/// axiom-eth caps the payload; alloy's `Encodable::length` includes the header.
fn rlp_list_payload_len(encoded_len: usize) -> usize {
    match encoded_len {
        0 => 0,
        1..=56 => encoded_len - 1,
        57..=257 => encoded_len - 2,
        _ => encoded_len.saturating_sub(3),
    }
}

/// Payload of an Ethereum log `[address, topics[], data]` — the list
/// body axiom-eth range-checks, header excluded.
pub fn encoded_log_len(topic_count: usize, data: &[u8]) -> usize {
    let address = 21;
    let topics_payload = 33 * topic_count;
    let topics = rlp_list_encoded_len(topics_payload);
    let data_enc = rlp_bytes_encoded_len(data);
    address + topics + data_enc
}

fn rlp_list_encoded_len(payload: usize) -> usize {
    if payload <= 55 {
        1 + payload
    } else if payload <= 255 {
        2 + payload
    } else if payload <= 65535 {
        3 + payload
    } else {
        4 + payload
    }
}

fn rlp_bytes_encoded_len(b: &[u8]) -> usize {
    match b {
        [] => 1,
        [x] if *x < 0x80 => 1,
        x if x.len() <= 55 => 1 + x.len(),
        x if x.len() <= 255 => 2 + x.len(),
        x if x.len() <= 65535 => 3 + x.len(),
        x => 4 + x.len(),
    }
}

/// Checks a transaction against the circuit's limits and the CLI's own
/// rule that it goes straight to `bridge`.
pub fn check_tx_shape(shape: &TxShape, bridge: Address) -> Result<(), ShapeViolation> {
    if shape.tx_type != EIP1559_TX_TYPE && shape.tx_type != EIP2930_TX_TYPE {
        return Err(ShapeViolation::NotTypedTx {
            tx_type: shape.tx_type,
        });
    }
    if shape.input_len > MAX_TX_CALLDATA_BYTE_LEN {
        return Err(ShapeViolation::CalldataTooLong {
            len: shape.input_len,
        });
    }
    let al_payload = rlp_list_payload_len(shape.access_list_rlp_len);
    if al_payload > access_list_max(shape.tx_type) {
        return Err(ShapeViolation::AccessListTooLong {
            len: al_payload,
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
///
/// `max_encoded_log_len` is the longest **payload** of any log, not
/// only the Deposit event: axiom-eth range-checks every log's payload.
pub fn check_receipt_bounds(
    log_count: usize,
    max_encoded_log_len: usize,
) -> Result<(), ShapeViolation> {
    if log_count > MAX_RECEIPT_LOGS {
        return Err(ShapeViolation::TooManyLogs {
            count: log_count,
        });
    }
    if max_encoded_log_len > MAX_ENCODED_LOG_LEN {
        return Err(ShapeViolation::EncodedLogTooLong {
            len: max_encoded_log_len,
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
    fn type_1_and_type_2_are_provable() {
        for ty in [0u8, 4] {
            assert_eq!(
                check_tx_shape(&shape(ty, 100, 1), BRIDGE),
                Err(ShapeViolation::NotTypedTx {
                    tx_type: ty
                })
            );
        }
        assert_eq!(check_tx_shape(&shape(1, 100, 1), BRIDGE), Ok(()));
        assert_eq!(check_tx_shape(&shape(2, 100, 1), BRIDGE), Ok(()));
    }

    #[test]
    fn calldata_and_access_list_bounds_are_inclusive() {
        // alloy reports encoded length (header included). Payload 512 is
        // over 255 B, so the header is 3 bytes and the encoding is 515.
        assert_eq!(check_tx_shape(&shape(2, 2048, 515), BRIDGE), Ok(()));
        assert_eq!(
            check_tx_shape(&shape(2, 2049, 1), BRIDGE),
            Err(ShapeViolation::CalldataTooLong {
                len: 2049
            })
        );
        assert_eq!(
            check_tx_shape(&shape(2, 100, 516), BRIDGE),
            Err(ShapeViolation::AccessListTooLong {
                len: 513
            })
        );
        // Type 1: 2051 = 3-byte header + 2048 B payload.
        assert_eq!(check_tx_shape(&shape(1, 100, 2051), BRIDGE), Ok(()));
        assert_eq!(
            check_tx_shape(&shape(1, 100, 2052), BRIDGE),
            Err(ShapeViolation::AccessListTooLong {
                len: 2049
            })
        );
    }

    #[test]
    fn a_call_through_another_contract_is_refused() {
        let other = address!("5ff137d4b0fdcd49dca30c7cf57e578a026d2789");
        let mut s = shape(2, 612, 1);
        s.to = Some(other);
        assert_eq!(
            check_tx_shape(&s, BRIDGE),
            Err(ShapeViolation::NotToBridge {
                to: Some(other)
            })
        );
    }

    #[test]
    fn receipt_bounds_match_the_prover_parser() {
        assert_eq!(check_receipt_bounds(20, MAX_ENCODED_LOG_LEN), Ok(()));
        assert_eq!(
            check_receipt_bounds(21, 128),
            Err(ShapeViolation::TooManyLogs {
                count: 21
            })
        );
        assert_eq!(
            check_receipt_bounds(2, MAX_ENCODED_LOG_LEN + 1),
            Err(ShapeViolation::EncodedLogTooLong {
                len: MAX_ENCODED_LOG_LEN + 1
            })
        );
        // Four topics + 2051 B of data: payload 2209, encoding 2212 —
        // axiom-eth accepts the payload. Four topics + 2054 B: 2212.
        assert_eq!(encoded_log_len(4, &[0u8; 2051]), 2209);
        assert_eq!(
            check_receipt_bounds(2, encoded_log_len(4, &[0u8; 2051])),
            Ok(())
        );
        assert_eq!(encoded_log_len(4, &[0u8; 2054]), 2212);
        assert_eq!(
            check_receipt_bounds(2, encoded_log_len(4, &[0u8; 2054])),
            Err(ShapeViolation::EncodedLogTooLong {
                len: 2212
            })
        );
    }

    /// The limits are copied from the circuit, which lives in another
    /// cargo tree. This reads the circuit's source and fails when the
    /// numbers move there and not here.
    #[test]
    fn limits_match_the_circuit_source() {
        let circuit = include_str!("../../../../deposit-prover/src/circuit_v2.rs");
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
        assert_eq!(konst(circuit, "EIP2930_TX_TYPE"), EIP2930_TX_TYPE as u64);
        assert_eq!(
            konst(circuit, "PRODUCTION_MAX_DATA_BYTE_LEN"),
            MAX_LOG_DATA_BYTE_LEN as u64
        );
        assert!(
            circuit.contains("pub const fn max_encoded_log_len"),
            "the encoded-log formula moved"
        );
        assert_eq!(
            MAX_ENCODED_LOG_LEN,
            3 + 21 + 3 + 33 * MAX_LOG_TOPICS + 3 + MAX_LOG_DATA_BYTE_LEN + 1
        );
        assert_eq!(
            konst(circuit, "PRODUCTION_MAX_LOG_NUM"),
            MAX_RECEIPT_LOGS as u64
        );
        let provable = include_str!("../../../../deposit-prover/src/provable.rs");
        assert_eq!(
            konst(provable, "UNPROVABLE_EXIT_CODE"),
            PROVER_UNPROVABLE_EXIT as u64
        );
    }
}
