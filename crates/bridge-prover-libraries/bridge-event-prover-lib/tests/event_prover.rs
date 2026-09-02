//! Plumbing tests for `bridge_event_prover_lib::build_proof_inputs`.
//!
//! The Circuit-4 MockProver test that *exercises constraint satisfaction*
//! already lives in the upstream circuits crate
//! (`bridge-event-prove-circuit::tests::test_bridge_event_prove_circuit_for_all_collected_events_mock_prover`).
//! These tests focus on what Track C actually owns: the JSON-schema →
//! `BridgeEventProveCircuit` + public-instance-vector translation, plus the
//! validation guardrails `build_proof_inputs` enforces against an
//! ill-formed daemon-side anchor.

use bridge_event_witness::{
    export_from_event_boc_base64,
    schema::{
        AnchorRef, DenseChainLinkSer, MerkleProofData, PrivateWitness, SCHEMA_VERSION,
    },
    BlockContextInput,
};
use bridge_event_prove_circuit::bridge_event_prove_circuit::TOTAL_PUBLIC_INPUTS;
use bridge_event_prover_lib::{build_proof_inputs, default_event_circuit_params};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

// Reused from the test_withdrawal.rs fixture — first record of the
// upstream `withdrawals.txt`. Inlining keeps the test hermetic.
const EVENT_BOC_B64: &str = "te6ccgEBBAEAyQABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAAIZdmoHXvdgAQJwPIOJWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAA9CQAAAAAIDAgBDgAm3yq/MUPIYsiAFU9xmlVK1j7ShCFBTfqlHoaqgQg+0MAAodC01zGY0wFMpJaO4RLxFTkQ49E4=";
// NOTE: keep this string identical to bridge-event-witness/tests/test_withdrawal.rs.

const EXPECTED_TOKEN_ID: u32 = 2;

// Constants matching `gosh_dense_balanced_tree::MAX_CHAIN_LEN` at the
// pinned revision. If the upstream bumps this we want a loud failure here.
const MAX_CHAIN_LEN_EXPECTED: usize = 11;

/// Build a fully-populated `PrivateWitness` from the captured fixture by
/// running the per-tx exporter, then filling in plausible daemon-side
/// fields (`events_tree_proof`, `block_tree_proof`, `anchor`) with shape-
/// correct (but not constraint-satisfying) values.
///
/// The data here will NOT satisfy the circuit — that's intentional. These
/// tests verify the translation layer, not the cryptographic content. The
/// circuit MockProver test in the circuits crate covers constraint
/// satisfaction with real Poseidon-consistent witnesses.
fn populated_witness() -> PrivateWitness {
    let ctx = BlockContextInput {
        block_id: [0x11u8; 32],
        block_seq_no: 12345,
        account_dapp_id: [0x22u8; 32],
        account_id: [0x33u8; 32],
        envelope_hash: [0x44u8; 32],
    };

    let mut w = export_from_event_boc_base64(EVENT_BOC_B64, &ctx).expect("exporter must succeed");

    // Synthesise an events-tree proof of depth ceil(log2(128)) = 7.
    let events_tree_proof = MerkleProofData {
        position: 0,
        siblings_hex: (0..7).map(|i| hex::encode([i as u8; 32])).collect(),
    };

    // Block tree depth ceil(log2(130)) = 8.
    let block_tree_proof = MerkleProofData {
        position: 0,
        siblings_hex: (0..8).map(|i| hex::encode([(0x80 | i) as u8; 32])).collect(),
    };

    let chosen_layer_hash = hex::encode([0xCAu8; 32]);

    // Pad an active first link + (MAX_CHAIN_LEN_EXPECTED - 1) inactive
    // links. Inactive sibling depth matches the active link.
    let active_link = DenseChainLinkSer {
        active: true,
        position: 0,
        siblings_hex: (0..8).map(|i| hex::encode([(0xA0 | i) as u8; 32])).collect(),
        leaf_hex: hex::encode([0xEEu8; 32]),
    };
    let inactive_link = DenseChainLinkSer {
        active: false,
        position: 0,
        siblings_hex: vec![hex::encode([0u8; 32]); 8],
        leaf_hex: hex::encode([0xFFu8; 32]),
    };
    let mut dense_chain = Vec::with_capacity(MAX_CHAIN_LEN_EXPECTED);
    dense_chain.push(active_link);
    while dense_chain.len() < MAX_CHAIN_LEN_EXPECTED {
        dense_chain.push(inactive_link.clone());
    }

    let anchor = AnchorRef {
        layer_idx: 0,
        height: 9999,
        layer_hash_hex: chosen_layer_hash,
        dense_chain,
        num_active_chain_steps: 1,
    };

    w.events_tree_proof = Some(events_tree_proof);
    w.block_tree_proof = Some(block_tree_proof);
    w.anchor = Some(anchor);
    w
}

#[test]
fn happy_path_translates_to_circuit_inputs() {
    let w = populated_witness();
    let inputs = build_proof_inputs(&w, default_event_circuit_params())
        .expect("build_proof_inputs must succeed on fully-populated witness");

    // public_instances layout (10 slots, see event_verifier.rs):
    //   [token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
    //    sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root]
    assert_eq!(inputs.public_instances.len(), TOTAL_PUBLIC_INPUTS);
    assert_eq!(inputs.public_instances[0], Fr::from(EXPECTED_TOKEN_ID as u64));

    // The remaining slots come from BE/LE byte-packing of the fixture
    // data; we don't recompute them here, but we assert they're distinct
    // from the zero-byte default so we know the translation actually
    // ran. (Slots 1..=9 are amount, recipient_hi, recipient_lo,
    // dst_chain_id, sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root.)
    for slot in 1..TOTAL_PUBLIC_INPUTS {
        assert_ne!(
            inputs.public_instances[slot],
            Fr::zero(),
            "public-instance slot {slot} must be non-zero with our fixture seeding"
        );
    }
}

/// `EventProofInputs` does not implement `Debug`, so `Result::expect_err`
/// is unavailable. This helper pulls the error out without that bound.
fn must_err(w: &PrivateWitness, ctx: &str) -> anyhow::Error {
    match build_proof_inputs(w, default_event_circuit_params()) {
        Ok(_) => panic!("{ctx}: expected error, got Ok"),
        Err(e) => e,
    }
}

#[test]
fn schema_version_mismatch_errors() {
    let mut w = populated_witness();
    w.schema_version = SCHEMA_VERSION + 1;
    let err = must_err(&w, "schema version");
    let msg = format!("{err}");
    assert!(
        msg.contains("schema_version"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn missing_anchor_errors() {
    let mut w = populated_witness();
    w.anchor = None;
    let err = must_err(&w, "missing anchor");
    assert!(format!("{err}").contains("anchor missing"));
}

#[test]
fn missing_events_tree_proof_errors() {
    let mut w = populated_witness();
    w.events_tree_proof = None;
    let err = must_err(&w, "missing events_tree_proof");
    assert!(format!("{err}").contains("events_tree_proof missing"));
}

#[test]
fn missing_block_tree_proof_errors() {
    let mut w = populated_witness();
    w.block_tree_proof = None;
    let err = must_err(&w, "missing block_tree_proof");
    assert!(format!("{err}").contains("block_tree_proof missing"));
}

#[test]
fn dense_chain_wrong_length_errors() {
    let mut w = populated_witness();
    let anchor = w.anchor.as_mut().unwrap();
    anchor.dense_chain.pop();
    let err = must_err(&w, "undersized dense_chain");
    assert!(format!("{err}").contains("MAX_CHAIN_LEN"));
}
