//! TD-02 — PI layout drift: prover 12 PI vs legacy AN 8-Fr parser offsets.
//!
//! Catalog `TD-02`: prover layout places `chainId` at index 4; legacy
//! `USDCBridge._parsePublicInputs` reads 8 Fr and maps `fr[4..5]` → dappId,
//! `fr[6..7]` → anAccount.

use alloy::primitives::U256;
use deposit_relayer_daemon::types::{DepositPublicInputs, NUM_PUBLIC_INPUTS};

const LAYOUT: [&str; 12] = [
    "depositId",
    "sender",
    "amount",
    "contractAddress",
    "chainId",
    "dappIdHigh",
    "dappIdLow",
    "anAccountHigh",
    "anAccountLow",
    "blockHashHigh",
    "blockHashLow",
    "promiseCommit",
];

fn words_from_operand(pi: &DepositPublicInputs) -> Vec<U256> {
    let bytes = pi.to_operand();
    (0..NUM_PUBLIC_INPUTS)
        .map(|i| {
            let mut le = [0u8; 32];
            le.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
            U256::from_le_bytes(le)
        })
        .collect()
}

/// Legacy AN parser (8 Fr) — mirrors `audit/spec/an-contracts/exchange/USDCBridge.sol`.
fn parse_an_legacy_eight_fr(words: &[U256]) -> (U256, U256, U256) {
    let dapp_id = (words[4] << 128) | words[5];
    let an_account = (words[6] << 128) | words[7];
    (words[0], dapp_id, an_account)
}

fn synthetic_pi() -> DepositPublicInputs {
    DepositPublicInputs {
        deposit_id: U256::from(7u64),
        sender: U256::from(0x1234u64),
        amount: U256::from(1_000_000u64),
        contract_address: U256::from(0xabcdu64),
        chain_id: U256::from(11_155_111u64),
        dapp_id_high: U256::from(0xaaaa_bbbbu64),
        dapp_id_low: U256::from(0xcccc_ddddu64),
        an_account_high: U256::from(0x1111_2222u64),
        an_account_low: U256::from(0x3333_4444u64),
        block_hash_high: U256::from(0xdead_beefu64),
        block_hash_low: U256::from(0xcafe_babeu64),
        promise_commit: U256::from(0x99u64),
    }
}

#[test]
fn td_02_layout_table_has_twelve_slots_with_chain_id_at_four() {
    assert_eq!(LAYOUT.len(), NUM_PUBLIC_INPUTS);
    assert_eq!(LAYOUT[4], "chainId");
}

#[test]
fn td_02_legacy_an_parser_misreads_prover_twelve_pi_operand() {
    let pi = synthetic_pi();
    let words = words_from_operand(&pi);
    assert_eq!(words.len(), 12);

    let prover_dapp = (pi.dapp_id_high << 128) | pi.dapp_id_low;
    let prover_acct = (pi.an_account_high << 128) | pi.an_account_low;
    let (_, an_dapp, an_acct) = parse_an_legacy_eight_fr(&words);

    assert_ne!(pi.chain_id, U256::ZERO);
    assert_eq!(
        an_dapp,
        (pi.chain_id << 128) | pi.dapp_id_high,
        "legacy parser maps fr[4..5] to dappId (= chainId|dappHigh in 12-PI layout)"
    );
    assert_ne!(an_acct, prover_acct, "legacy anAccount drifts from prover slots 7/8");
    let _ = prover_dapp;
}
