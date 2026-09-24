//! Reader for the captured `WithdrawalInitiated` events fixture
//! (`withdrawals.txt`), produced by
//! `acki-nacki/tests/exchange/generate_withdrawals.py`.
//!
//! File format: repeating groups of 7 lines + 1 blank separator:
//!   line 1: dst_chain_id   (decimal u256, fits in u128 for fixtures)
//!   line 2: recipient_hex  (lowercase hex, length == 2*recipient_len bytes)
//!   line 3: amount         (decimal u128)
//!   line 4: token_id       (decimal u32)
//!   line 5: sender_addr    (TVM address string `wc:account_id_hex`)
//!   line 6: event_boc      (base64 of the full ExtOut `Message`)
//!   line 7: event_block_id (hex, 64 chars)
//!   line 8: <blank>

use std::fs;
use std::path::Path;

/// One captured `WithdrawalInitiated` event, as recorded by the generator.
pub struct WithdrawalRecord {
    pub dst_chain_id: u128,
    pub recipient_hex: String,
    pub amount: u128,
    pub token_id: u32,
    pub sender_addr: String,
    pub event_boc: String,
    pub event_block_id_hex: String,
}

/// Parse `withdrawals.txt`. Lenient about trailing blank lines.
pub fn read_withdrawals_from_file(path: impl AsRef<Path>) -> Vec<WithdrawalRecord> {
    let content =
        fs::read_to_string(path).expect("failed to read withdrawals fixture file");
    let lines: Vec<&str> = content.lines().collect();

    let mut records = Vec::new();
    let mut i = 0;
    while i + 6 < lines.len() {
        // Skip stray blank lines between records / at top of file.
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }

        let dst_chain_id = lines[i]
            .trim()
            .parse::<u128>()
            .expect("dst_chain_id line is not a u128");
        let recipient_hex = lines[i + 1].trim().to_string();
        let amount = lines[i + 2]
            .trim()
            .parse::<u128>()
            .expect("amount line is not a u128");
        let token_id = lines[i + 3]
            .trim()
            .parse::<u32>()
            .expect("token_id line is not a u32");
        let sender_addr = lines[i + 4].trim().to_string();
        let event_boc = lines[i + 5].trim().to_string();
        let event_block_id_hex = lines[i + 6].trim().to_string();

        records.push(WithdrawalRecord {
            dst_chain_id,
            recipient_hex,
            amount,
            token_id,
            sender_addr,
            event_boc,
            event_block_id_hex,
        });

        // Step past the 7 data lines + 1 blank.
        i += 8;
    }

    records
}
