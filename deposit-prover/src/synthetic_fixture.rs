//! Synthetic [`DepositProofInput`] for keygen, MockProver, and `deposit_10proofs` regen.
//!
//! Uses [`crate::rlp_utils::encode_receipt`] (legacy, no EIP-2718 type prefix) so
//! axiom-eth @ `1d61be0` decomposes exactly four receipt fields (QC-PROV-02).

use ethers::types::{Address, Bloom, Bytes, H256, Log, TransactionReceipt, U256, U64};

use crate::{
    ethereum_fetcher::get_deposit_event_signature,
    mpt::receipt_proof_from_receipt,
    prover::CircuitConfig,
    types::{DepositEventData, DepositProofInput},
};

/// Circuit params aligned with `export_deposit_proof_set` / audit fixtures.
pub fn audit_circuit_config() -> CircuitConfig {
    CircuitConfig {
        degree: 18,
        max_data_byte_len: 256,
        max_log_num: 20,
        topic_num_bounds: (0, 4),
    }
}

/// Minimal single-log deposit witness; `deposit_id` is reflected in event + log topics.
pub fn synthetic_deposit_proof_input(deposit_id: u64) -> DepositProofInput {
    let sender = [0x11u8; 20];
    let contract = [0x22u8; 20];
    let mut amount = [0u8; 32];
    amount[31] = ((deposit_id + 1) * 1_000).min(255) as u8;
    if amount[31] == 0 {
        amount[31] = 1;
    }
    let an_account = [0x33u8; 32];
    let timestamp = 1_700_000_000u64 + deposit_id;

    let mut sender_topic = [0u8; 32];
    sender_topic[12..32].copy_from_slice(&sender);

    let mut data = [0u8; 128];
    data[0..32].copy_from_slice(&amount);
    data[63] = 0; // anWorkchain
    data[64..96].copy_from_slice(&an_account);
    let mut ts_word = [0u8; 32];
    U256::from(timestamp).to_big_endian(&mut ts_word);
    data[96..128].copy_from_slice(&ts_word);

    let log = Log {
        address: Address::from_slice(&contract),
        topics: vec![
            H256::from(get_deposit_event_signature()),
            H256::from_low_u64_be(deposit_id),
            H256(sender_topic),
        ],
        data: Bytes::from(data.to_vec()),
        ..Default::default()
    };

    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::zero(),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(1)),
        from: Address::from_slice(&sender),
        to: Some(Address::from_slice(&contract)),
        cumulative_gas_used: U256::from(21_000),
        gas_used: Some(U256::from(21_000)),
        contract_address: None,
        logs: vec![log],
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: None,
        effective_gas_price: None,
        other: Default::default(),
    };

    let receipt_proof = receipt_proof_from_receipt(&receipt)
        .expect("synthetic receipt trie proof must build");

    let event_data = DepositEventData {
        block_number: 1,
        transaction_index: 0,
        log_index: 0,
        deposit_id,
        sender,
        amount,
        an_workchain: 0,
        an_account,
        timestamp,
        contract_address: contract,
    };

    DepositProofInput {
        event_data,
        receipt_proof,
        dapp_id: [0u8; 32],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{prover::test_circuit_mock, synthetic_fixture::audit_circuit_config};

    #[test]
    fn synthetic_input_satisfies_mock_prover_audit_config() {
        let input = synthetic_deposit_proof_input(0);
        test_circuit_mock(input, &audit_circuit_config())
            .expect("synthetic legacy receipt must satisfy MockProver");
    }
}
