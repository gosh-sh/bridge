//! TD-09 — MPT differential + systematic witness mutation (deposit-prover).
//!
//! Catalog `TD-09`: mutate receipt trie witness on committed `proof_00` fixture;
//! expect fail-closed MockProver reject except happy path.

use deposit_prover::{
    audit_circuit_config,
    circuit_v2::PI_CHAIN_ID,
    test_circuit_mock_instances,
    test_circuit_mock_with_mpt_mutation,
    types::DepositProofInput,
    MptWitnessMutation,
};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

const SEPOLIA: u64 = 11_155_111;

fn sepolia_proof_00() -> DepositProofInput {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json");
    let json = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn horner_fr(bytes: &[u8]) -> Fr {
    let mut acc = Fr::zero();
    let base = Fr::from(256u64);
    for &byte in bytes {
        acc = acc * base + Fr::from(byte as u64);
    }
    acc
}

fn address_to_fr(addr: &[u8; 20]) -> Fr {
    let mut padded = [0u8; 32];
    padded[12..].copy_from_slice(addr);
    horner_fr(&padded)
}

fn expect_mpt_mutation_rejected(mutation: MptWitnessMutation) {
    let input = sepolia_proof_00();
    let err = test_circuit_mock_with_mpt_mutation(input, &audit_circuit_config(), Some(mutation))
        .unwrap_err();
    assert!(
        err.contains("not satisfied") || err.contains("MockProver"),
        "TD-09: expected circuit reject, got: {err}"
    );
}

#[test]
fn td_09_proof_00_happy_path_pi_matches_event() {
    let input = sepolia_proof_00();
    let instances =
        test_circuit_mock_instances(input.clone(), &audit_circuit_config(), None).unwrap();
    let pis = &instances[0];

    assert_eq!(pis[0], Fr::from(input.event_data.deposit_id));
    assert_eq!(pis[1], address_to_fr(&input.event_data.sender));
    assert_eq!(pis[2], horner_fr(&input.event_data.amount));
    assert_eq!(pis[PI_CHAIN_ID], Fr::from(SEPOLIA));
    assert_eq!(input.event_data.deposit_id, 0);
}

#[test]
fn td_09_corrupt_receipt_root_rejected() {
    expect_mpt_mutation_rejected(MptWitnessMutation {
        flip_receipt_root_byte: Some((0, 0xFF)),
        ..Default::default()
    });
}

#[test]
fn td_09_swap_receipt_proof_nodes_rejected() {
    expect_mpt_mutation_rejected(MptWitnessMutation {
        swap_receipt_proof_nodes: Some((0, 1)),
        ..Default::default()
    });
}

#[test]
fn td_09_truncated_receipt_rlp_rejected() {
    let input = sepolia_proof_00();
    let truncate_len = input.receipt_proof.receipt_rlp.len() / 2;
    expect_mpt_mutation_rejected(MptWitnessMutation {
        truncate_receipt_rlp: Some(truncate_len),
        ..Default::default()
    });
}

#[test]
fn td_09_substitute_sibling_proof_node_rejected() {
    expect_mpt_mutation_rejected(MptWitnessMutation {
        substitute_receipt_proof_node: Some((1, 3)),
        ..Default::default()
    });
}

#[test]
fn td_09_corrupt_proof_node_body_byte_rejected() {
    expect_mpt_mutation_rejected(MptWitnessMutation {
        corrupt_receipt_proof_node_byte: Some((0, 8, 0x00)),
        ..Default::default()
    });
}
