//! F10 property tests — PI roundtrip, finalize encode/decode, confirmation depth,
//! relayer state machine, backoff policy.
//!
//! Gate: `cd crates/deposit-relayer-daemon && cargo test --test f10_proptest`

use std::time::Duration;

use alloy::primitives::{Bytes, U256};
use deposit_relayer_daemon::{
    decode_finalize_deposit, encode_finalize_deposit, is_deposit_block_finalized,
    BackoffConfig, DepositProofBundle, DepositPublicInputs, RelayerState,
};
use proptest::prelude::*;

fn arb_u256() -> impl Strategy<Value = U256> {
    any::<[u8; 32]>().prop_map(U256::from_le_bytes)
}

fn arb_deposit_public_inputs() -> impl Strategy<Value = DepositPublicInputs> {
    (
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
        arb_u256(),
    )
        .prop_map(
            |(
                deposit_id,
                sender,
                amount,
                contract_address,
                chain_id,
                dapp_id_high,
                dapp_id_low,
                an_account_high,
                an_account_low,
                block_hash_high,
                block_hash_low,
                promise_commit,
            )| {
                DepositPublicInputs {
                    deposit_id,
                    sender,
                    amount,
                    contract_address,
                    chain_id,
                    dapp_id_high,
                    dapp_id_low,
                    an_account_high,
                    an_account_low,
                    block_hash_high,
                    block_hash_low,
                    promise_commit,
                }
            },
        )
}

fn arb_proof_bundle() -> impl Strategy<Value = DepositProofBundle> {
    (
        arb_deposit_public_inputs(),
        prop::collection::vec(any::<u8>(), 0..512usize),
        prop::collection::vec(any::<u8>(), 16..128usize),
    )
        .prop_map(|(parsed, vk_blob, proof)| DepositProofBundle {
            vk_blob: Bytes::from(vk_blob),
            public_inputs: Bytes::from(parsed.to_operand()),
            proof: Bytes::from(proof),
            parsed,
        })
}

proptest! {
    #[test]
    fn public_inputs_operand_roundtrip(pi in arb_deposit_public_inputs()) {
        let bytes = pi.to_operand();
        let decoded = DepositPublicInputs::from_operand(&bytes).unwrap();
        prop_assert_eq!(decoded, pi);
    }

    #[test]
    fn finalize_encode_decode_roundtrip(bundle in arb_proof_bundle()) {
        let body = encode_finalize_deposit(&bundle);
        let (scalars, proof) = decode_finalize_deposit(&body).unwrap();
        prop_assert_eq!(proof, bundle.proof.as_ref());
        let expected = [
            bundle.parsed.deposit_id,
            bundle.parsed.sender,
            bundle.parsed.amount,
            bundle.parsed.contract_address,
            bundle.parsed.chain_id,
            bundle.parsed.dapp_id_high,
            bundle.parsed.dapp_id_low,
            bundle.parsed.an_account_high,
            bundle.parsed.an_account_low,
            bundle.parsed.block_hash_high,
            bundle.parsed.block_hash_low,
            bundle.parsed.promise_commit,
        ];
        prop_assert_eq!(scalars, expected);
    }

    #[test]
    fn confirmation_depth_matches_safe_head(
        deposit_block in any::<u64>(),
        chain_head in any::<u64>(),
        confirmations in any::<u64>(),
    ) {
        let safe_head = chain_head.saturating_sub(confirmations);
        let expected = deposit_block <= safe_head;
        prop_assert_eq!(
            is_deposit_block_finalized(deposit_block, chain_head, confirmations),
            expected
        );
    }

    #[test]
    fn confirmation_monotonic_in_head(
        deposit_block in any::<u64>(),
        chain_head in 1u64..u64::MAX,
        confirmations in any::<u64>(),
    ) {
        let at_head = is_deposit_block_finalized(deposit_block, chain_head, confirmations);
        let at_next = is_deposit_block_finalized(deposit_block, chain_head + 1, confirmations);
        prop_assert!(!at_next || at_head, "higher head cannot un-finalize a block");
    }

    #[test]
    fn relayer_state_progress_sets_next_target(
        ids in prop::collection::vec(0u64..10_000, 1..30),
    ) {
        let mut s = RelayerState::default();
        for id in ids {
            s.record_progress(id);
            prop_assert_eq!(s.next_target(0), id.saturating_add(1));
            prop_assert_eq!(s.attempts_since_progress, 0);
            prop_assert_eq!(s.last_processed_deposit_id, Some(id));
        }
    }

    #[test]
    fn relayer_state_attempts_accumulate(
        deposit_id in 1u64..10_000,
        n in 1u32..100,
    ) {
        let mut s = RelayerState::default();
        for _ in 0..n {
            s.record_attempt(deposit_id);
        }
        prop_assert_eq!(s.attempts_since_progress, n);
        prop_assert_eq!(s.last_attempt_deposit_id, Some(deposit_id));
        prop_assert!(s.last_processed_deposit_id.is_none());
    }

    #[test]
    fn backoff_validate_rejects_zero_initial(initial_ms in 0u64..1) {
        let cfg = BackoffConfig {
            initial: Duration::from_millis(initial_ms),
            max: Duration::from_secs(60),
            multiplier: 2,
        };
        prop_assert!(cfg.validate().is_err());
    }

    #[test]
    fn backoff_validate_rejects_zero_multiplier(
        initial_ms in 1u64..10_000,
        max_ms in 10_000u64..60_000,
    ) {
        let cfg = BackoffConfig {
            initial: Duration::from_millis(initial_ms),
            max: Duration::from_millis(max_ms),
            multiplier: 0,
        };
        prop_assert!(cfg.validate().is_err());
    }
}
