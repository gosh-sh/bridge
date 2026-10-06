//! RPC `logIndex` counts logs across the whole block; the prover reads
//! `receipt.logs[i]`. The two differ whenever an earlier transaction in
//! the block emitted logs, so both are kept and only the receipt-local
//! one is passed to `fetch_deposit_data --log-index`.

/// Map a block-wide log index to its position within the receipt.
///
/// The RPC provides `logIndex` which counts logs across the entire block,
/// but the prover needs the position within the receipt. This function finds
/// the receipt-local index by searching for the block index in the slice of
/// receipt logs.
pub fn receipt_log_index(
    block_log_indices: &[Option<u64>],
    block_log_index: u64,
) -> Result<u64, String> {
    block_log_indices
        .iter()
        .position(|i| *i == Some(block_log_index))
        .map(|p| p as u64)
        .ok_or_else(|| {
            format!(
                "log {block_log_index} of the block is not in this receipt ({} logs)",
                block_log_indices.len()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relayer's regression fixture: a real Sepolia deposit whose
    /// block-wide `logIndex` differs from its position in the receipt.
    #[test]
    fn the_relayer_fixture_maps_the_same_way() {
        let raw =
            include_str!("../../../deposit-relayer-daemon/tests/fixtures/sepolia_deposit_id0.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let indices: Vec<Option<u64>> = v["receipt_logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                l["logIndex"]
                    .as_str()
                    .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
            })
            .collect();
        let got = receipt_log_index(&indices, v["block_log_index"].as_u64().unwrap()).unwrap();
        assert_eq!(got, v["expected_receipt_log_index"].as_u64().unwrap());
    }

    #[test]
    fn logs_of_earlier_transactions_shift_the_block_index_only() {
        // Receipt of the 3rd transaction: its logs are 7 and 8 in the block.
        assert_eq!(receipt_log_index(&[Some(7), Some(8)], 8), Ok(1));
        assert_ne!(receipt_log_index(&[Some(7), Some(8)], 8), Ok(8));
    }

    #[test]
    fn a_block_index_not_in_the_receipt_is_an_error() {
        assert!(receipt_log_index(&[Some(7), Some(8)], 9).is_err());
        assert!(receipt_log_index(&[None, None], 0).is_err());
    }
}
