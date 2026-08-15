//! TD-15 — `depositId` collision without `srcChainId` in voucher identity (multi-L2).
//!
//! Catalog `TD-15` / DEP-N-5: replay key must be
//! `(srcChainId, depositId, contractAddr, dappId)` — see
//! `scripts/check_voucher_abi_consistency.py` and patch
//! `USDCBridge_12pi_chainid_allowlist.patch`.

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    submitter::{AnSubmitter, MockAnSubmitter, SubmitOutcome, VoucherIdentity},
    types::{DepositEvent, DepositProofBundle},
};

const SEPOLIA: u64 = 11_155_111;
const BASE: u64 = 8_453;
const SHARED_DEPOSIT_ID: u64 = 42;
const SHARED_CONTRACT: Address = Address::repeat_byte(0x22);
const SHARED_DAPP: U256 = U256::from_limbs([0x0d15_5000, 0, 0, 0]);

fn deposit_on_chain(source_chain_id: u64) -> DepositEvent {
    DepositEvent {
        deposit_id: SHARED_DEPOSIT_ID,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(2_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::from(1_700_000_400u64),
        tx_hash: B256::repeat_byte(0xaa),
        log_index: 0,
        block_number: 200,
        block_hash: B256::repeat_byte(0xcd),
        source_contract: SHARED_CONTRACT,
        source_chain_id,
    }
}

fn bundle_for(event: &DepositEvent) -> DepositProofBundle {
    let parsed = MockProofGenerator::derive_public_inputs(event, SHARED_DAPP);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b, 0x00, 0x00].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde, 0xad].into(),
        parsed,
    }
}

#[tokio::test]
async fn td_15_cross_chain_same_deposit_id_distinct_vouchers_dep_n5() {
    let submitter = MockAnSubmitter::accepting_dep_n5();
    let sepolia = deposit_on_chain(SEPOLIA);
    let base = deposit_on_chain(BASE);
    let bundle_sepolia = bundle_for(&sepolia);
    let bundle_base = bundle_for(&base);

    assert_ne!(
        bundle_sepolia.parsed.chain_id,
        bundle_base.parsed.chain_id,
        "PI chainId must differ across chains"
    );
    assert_eq!(bundle_sepolia.parsed.deposit_id, bundle_base.parsed.deposit_id);

    assert!(matches!(
        submitter.submit(&sepolia, &bundle_sepolia).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert!(matches!(
        submitter.submit(&base, &bundle_base).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert_eq!(submitter.finalized_count(), 2, "DEP-N-5: two distinct mints");
}

#[tokio::test]
async fn td_15_same_chain_replay_rejects_second_finalize() {
    let submitter = MockAnSubmitter::accepting_dep_n5();
    let event = deposit_on_chain(SEPOLIA);
    let bundle = bundle_for(&event);

    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    assert!(matches!(
        submitter.submit(&event, &bundle).await.unwrap(),
        SubmitOutcome::AlreadyFinalized
    ));
    assert_eq!(submitter.finalized_count(), 1);
}

#[tokio::test]
async fn td_15_legacy_deposit_id_only_nullifier_collides_cross_chain() {
    let submitter = MockAnSubmitter::accepting();
    let sepolia = deposit_on_chain(SEPOLIA);
    let base = deposit_on_chain(BASE);

    assert!(matches!(
        submitter.submit(&sepolia, &bundle_for(&sepolia)).await.unwrap(),
        SubmitOutcome::Finalized { .. }
    ));
    match submitter.submit(&base, &bundle_for(&base)).await.unwrap() {
        SubmitOutcome::AlreadyFinalized => {},
        other => panic!("TD-15 legacy: cross-chain must not silently pass: {other:?}"),
    }
    assert_eq!(
        submitter.finalized_count(),
        1,
        "legacy depositId-only key swallows second chain (BC risk pre-DEP-N-5)"
    );
}

#[tokio::test]
async fn td_15_voucher_identity_matches_dep_n5_tuple() {
    let sepolia = deposit_on_chain(SEPOLIA);
    let base = deposit_on_chain(BASE);
    let b_sep = bundle_for(&sepolia);
    let b_base = bundle_for(&base);

    let id_sep = VoucherIdentity {
        chain_id: SEPOLIA,
        deposit_id: SHARED_DEPOSIT_ID,
        contract: SHARED_CONTRACT,
        dapp_id: SHARED_DAPP,
    };
    let id_base = VoucherIdentity {
        chain_id: BASE,
        deposit_id: SHARED_DEPOSIT_ID,
        contract: SHARED_CONTRACT,
        dapp_id: SHARED_DAPP,
    };

    assert_eq!(b_sep.parsed.chain_id, U256::from(SEPOLIA));
    assert_eq!(b_base.parsed.chain_id, U256::from(BASE));
    assert_ne!(id_sep, id_base);
}
