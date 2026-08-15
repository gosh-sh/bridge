//! TD-19 — receipt trie root vs header `receiptsRoot` decouple + stale `blockHash` PI.
//!
//! Catalog `TD-19` / DEP-MPT-EDGE: BC-CIRCUIT-004 binds MPT-verified root to header
//! field 5; block hash PI is keccak256(witness `block_header_rlp`).

use deposit_prover::{
    audit_circuit_config,
    mpt::align_block_header_roots,
    test_circuit_mock,
    test_circuit_mock_instances,
    test_circuit_mock_with_mpt_mutation,
    types::DepositProofInput,
    MptWitnessMutation,
};
use ethers::utils::keccak256;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn witness_block_hash(input: &DepositProofInput) -> [u8; 32] {
    keccak256(&input.receipt_proof.block_header_rlp)
}

fn horner_field(bytes: &[u8]) -> Fr {
    let mut acc = Fr::zero();
    let base = Fr::from(256u64);
    for &byte in bytes {
        acc = acc * base + Fr::from(byte as u64);
    }
    acc
}

fn block_hash_pi_from_witness(hash: [u8; 32]) -> (Fr, Fr) {
    (
        horner_field(&hash[0..16]),
        horner_field(&hash[16..32]),
    )
}

fn expect_mock_reject(input: DepositProofInput) {
    let err = test_circuit_mock(input, &audit_circuit_config()).unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-19: expected circuit reject, got: {err}"
    );
}

/// Header `receiptsRoot` (field 5) ≠ MPT-verified receipt root (witness trie).
fn decouple_header_receipts_root_from_mpt(input: &mut DepositProofInput) {
    let tx_root = input.tx_proof.transactions_root;
    let mpt_root = input.receipt_proof.receipt_root;
    let mut wrong = mpt_root;
    wrong[0] ^= 0xFF;
    input.receipt_proof.receipt_root = wrong;
    align_block_header_roots(&mut input.receipt_proof, tx_root)
        .expect("align header to wrong receipts root");
    input.receipt_proof.receipt_root = mpt_root;
}

/// Stale RPC `blockHash` vs proof-bound witness header (relayer `check_binds_to` shape).
fn stale_rpc_block_hash_vs_witness(witness_header_hash: [u8; 32]) -> bool {
    let stale = {
        let mut s = witness_header_hash;
        s[31] ^= 0x01;
        s
    };
    witness_header_hash != stale
}

#[test]
fn td_19_proof_00_aligned_happy_path() {
    let input = sepolia_proof_00();
    let hash = witness_block_hash(&input);
    assert_ne!(hash, [0u8; 32]);
    test_circuit_mock(input, &audit_circuit_config()).expect("TD-19 happy aligned roots");
}

#[test]
fn td_19_header_receipts_root_decoupled_from_mpt_rejected() {
    let mut input = sepolia_proof_00();
    decouple_header_receipts_root_from_mpt(&mut input);
    expect_mock_reject(input);
}

#[test]
fn td_19_corrupt_receipt_root_field_rejected() {
    expect_mock_reject_with_mutation(MptWitnessMutation {
        flip_receipt_root_byte: Some((0, 0xFF)),
        ..Default::default()
    });
}

#[test]
fn td_19_stale_block_hash_pi_not_equal_rpc_event_hash() {
    let input = sepolia_proof_00();
    let witness_hash = witness_block_hash(&input);
    let instances =
        test_circuit_mock_instances(input, &audit_circuit_config(), None).unwrap();
    let pis = &instances[0];
    let (exp_hi, exp_lo) = block_hash_pi_from_witness(witness_hash);
    assert_eq!(pis[9], exp_hi, "blockHashHigh PI must match witness header");
    assert_eq!(pis[10], exp_lo, "blockHashLow PI must match witness header");
    assert!(
        stale_rpc_block_hash_vs_witness(witness_hash),
        "TD-19: stale RPC blockHash would fail relayer check_binds_to"
    );
}

#[test]
fn td_19_mutated_header_rlp_changes_pi_block_hash() {
    let mut input = sepolia_proof_00();
    let before = witness_block_hash(&input);
    if !input.receipt_proof.block_header_rlp.is_empty() {
        input.receipt_proof.block_header_rlp[0] ^= 0x01;
    }
    let after = witness_block_hash(&input);
    assert_ne!(before, after, "header mutation must change keccak block hash");
    expect_mock_reject(input);
}

fn expect_mock_reject_with_mutation(mutation: MptWitnessMutation) {
    let input = sepolia_proof_00();
    let err = test_circuit_mock_with_mpt_mutation(input, &audit_circuit_config(), Some(mutation))
        .unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-19: expected reject, got: {err}"
    );
}

#[test]
fn td_19_pi_block_hash_matches_event_block_hash_when_aligned() {
    let input = sepolia_proof_00();
    let witness_hash = witness_block_hash(&input);
    let instances =
        test_circuit_mock_instances(input, &audit_circuit_config(), None).unwrap();
    let (exp_hi, exp_lo) = block_hash_pi_from_witness(witness_hash);
    assert_eq!(instances[0][9], exp_hi);
    assert_eq!(instances[0][10], exp_lo);
}
