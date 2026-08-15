//! TD-50 — relayer PI decode vs BN254 modulus; amount bind control.

use alloy::primitives::{Address, B256, U256};
use deposit_relayer_daemon::{
    prover::MockProofGenerator,
    types::{
        DepositEvent, DepositProofBundle, DepositPublicInputs, NUM_PUBLIC_INPUTS,
        PUBLIC_INPUT_BYTES,
    },
};

/// BN254 scalar field order (`AckiNackiBridge.BN254_R`).
const BN254_FR_MODULUS: U256 = U256::from_limbs([
    0x3e1f593f00000001,
    0x3e84879b9709143e,
    0x1585d2833e84879b,
    0x30644e72e131a029,
]);

const DAPP: U256 = U256::from_limbs([0x0a11_a000, 0, 0, 0]);

fn sample_event() -> DepositEvent {
    DepositEvent {
        deposit_id: 1,
        sender: Address::repeat_byte(0x11),
        amount: U256::from(1_000_000u64),
        an_workchain: 0,
        an_account: B256::repeat_byte(0x33),
        timestamp: U256::ZERO,
        tx_hash: B256::ZERO,
        log_index: 0,
        block_number: 1,
        block_hash: B256::ZERO,
        source_contract: Address::repeat_byte(0x22),
        source_chain_id: 11_155_111,
    }
}

fn bundle_for(event: &DepositEvent) -> DepositProofBundle {
    let parsed = MockProofGenerator::derive_public_inputs(event, DAPP);
    DepositProofBundle {
        vk_blob: vec![0x56, 0x4b].into(),
        public_inputs: parsed.to_operand().into(),
        proof: vec![0xde].into(),
        parsed,
    }
}

#[test]
fn td_50_check_binds_to_rejects_amount_mismatch_control() {
    let event = sample_event();
    let bundle = bundle_for(&event);
    let mut wrong = event.clone();
    wrong.amount = U256::from(2u64);
    assert!(
        bundle.check_binds_to(&wrong).is_err(),
        "amount mismatch must fail relayer bind"
    );
}

#[test]
fn td_50_relayer_compares_u256_not_fr_alias_class() {
    let event = sample_event();
    let bundle = bundle_for(&event);
    assert!(bundle.check_binds_to(&event).is_ok());
    // Relayer path: PI operand → U256 per slot; no second Fr-reduction pass.
    // Fr-alias at circuit layer (same Fr, different bytes) is relayer-invisible (QC).
    let mut wrong_ts = event.clone();
    wrong_ts.timestamp = U256::MAX;
    assert!(
        bundle.check_binds_to(&wrong_ts).is_ok(),
        "orthogonal: timestamp not bound — contrast with amount"
    );
}

#[test]
fn td_50_proof_00_operand_scalars_below_modulus() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../deposit-prover/fixtures/deposit_10proofs/proof_00/public_inputs.bin");
    let bytes = std::fs::read(&path).unwrap_or_else(|_| {
        let event = sample_event();
        MockProofGenerator::derive_public_inputs(&event, DAPP).to_operand()
    });
    assert_eq!(bytes.len(), PUBLIC_INPUT_BYTES);
    for i in 0..NUM_PUBLIC_INPUTS {
        let mut le = [0u8; 32];
        le.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
        let scalar = U256::from_le_bytes(le);
        assert!(
            scalar < BN254_FR_MODULUS,
            "slot {i} scalar {scalar:#x} must be canonical Fr < r"
        );
    }
}

#[test]
fn td_50_parsed_operand_round_trip_preserves_amount_u256() {
    let event = sample_event();
    let pi = MockProofGenerator::derive_public_inputs(&event, DAPP);
    let round = DepositPublicInputs::from_operand(&pi.to_operand()).unwrap();
    assert_eq!(round.amount, event.amount);
    assert_eq!(round.amount, pi.amount);
}
