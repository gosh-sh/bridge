//! TD-21 — relayer cross-check: `promiseCommit` is not event-bound at `check_binds_to`.

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::types::{DepositEvent, DepositProofBundle, DepositPublicInputs};

fn sample_event() -> DepositEvent {
    DepositEvent {
        deposit_id: 1,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(100u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::ZERO,
        tx_hash: B256::ZERO,
        log_index: 0,
        block_number: 1,
        block_hash: B256::ZERO,
        source_contract: Address::ZERO,
        source_chain_id: 11_155_111,
    }
}

fn sample_pi() -> DepositPublicInputs {
    DepositPublicInputs {
        deposit_id: U256::from(1u64),
        sender: U256::from_be_bytes::<32>({
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(Address::repeat_byte(0x11).as_slice());
            b
        }),
        amount: U256::from(100u64),
        contract_address: U256::ZERO,
        chain_id: U256::from(11_155_111u64),
        dapp_id_high: U256::ZERO,
        dapp_id_low: U256::ZERO,
        an_account_high: U256::from_be_slice(&[0x33u8; 16]),
        an_account_low: U256::from_be_slice(&[0x33u8; 16]),
        block_hash_high: U256::ZERO,
        block_hash_low: U256::ZERO,
        promise_commit: U256::from(0x1234u64),
    }
}

#[test]
fn td_21_mutated_promise_commit_not_checked_by_relayer_bind() {
    let event = sample_event();
    let mut pi = sample_pi();
    pi.promise_commit = U256::from(0xDEAD_BEEFu64);
    let bundle = DepositProofBundle {
        vk_blob: alloy::primitives::Bytes::from(vec![1u8; 4]),
        public_inputs: alloy::primitives::Bytes::from(pi.to_operand()),
        proof: alloy::primitives::Bytes::from(vec![2u8; 8]),
        parsed: pi,
    };
    // QC: relayer binds event fields; promiseCommit verified on AN via opcode + circuit.
    assert!(
        bundle.check_binds_to(&event).is_ok(),
        "TD-21: check_binds_to does not compare promiseCommit (AN/circuit binding)"
    );
}
