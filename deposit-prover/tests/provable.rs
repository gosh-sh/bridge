//! The keccak budget check on a real deposit, through the circuit's own
//! witness generation. Seconds per case; run from `deposit-prover/`:
//!
//! ```text
//! cargo test --release --test provable
//! ```

use deposit_prover::{
    mpt::proof_for_single_leaf,
    provable::{check_keccak_budget, check_provable},
    prover::CircuitConfig,
    rlp_utils::encode_tx_index,
    types::DepositProofInput,
};

fn load_proof_00() -> DepositProofInput {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/deposit_10proofs/proof_00/input.json"
    );
    let json = std::fs::read_to_string(path).expect("proof_00 input");
    serde_json::from_str(&json).expect("proof_00 json")
}

fn rlp_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    alloy_rlp::Encodable::encode(&b, &mut out);
    out
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = items.concat();
    let mut out = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: body.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&body);
    out
}

/// A type-2 receipt of `n` logs, each with four topics and `data` bytes.
fn receipt(n: usize, data: usize) -> Vec<u8> {
    let log = rlp_list(&[
        rlp_bytes(&[0x11; 20]),
        rlp_list(&vec![rlp_bytes(&[0x22; 32]); 4]),
        rlp_bytes(&vec![0x33; data]),
    ]);
    let mut out = vec![0x02];
    out.extend(rlp_list(&[
        rlp_bytes(&[1]),
        rlp_bytes(&[0x5a, 0x08]),
        rlp_bytes(&[0; 256]),
        rlp_list(&vec![log; n]),
    ]));
    out
}

#[test]
fn a_real_deposit_is_provable() {
    check_provable(&load_proof_00(), &CircuitConfig::production()).unwrap();
}

#[test]
fn a_receipt_within_every_log_bound_can_still_exceed_the_keccak_budget() {
    let mut input = load_proof_00();
    let big = receipt(20, 2048);
    let key = encode_tx_index(input.event_data.transaction_index);
    let (root, nodes) = proof_for_single_leaf(key, big.clone()).unwrap();
    input.receipt_proof.receipt_rlp = big;
    input.receipt_proof.receipt_root = root;
    input.receipt_proof.proof_nodes = nodes;
    let config = CircuitConfig::production();
    deposit_prover::provable::check_witness_bounds(&input, &config).unwrap();
    let e = check_keccak_budget(&input, &config).unwrap_err();
    assert!(e.0.contains("keccak permutations"), "{e}");
}
