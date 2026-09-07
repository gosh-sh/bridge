//! M3-exec tests: `hash_tree_root(ExecutionPayloadHeader)` + execution branch
//! against the real mainnet fixture (fork fulu).
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test execution_mock_prover -- --ignored --nocapture
//! ```
//!
//! Off-circuit: native exec-payload root + `execution_branch` @ gindex 25 must
//! reconstruct `finalized_header.beacon.body_root` (validates the 17-field
//! merkleization + gindex on live data). In-circuit MockProver proves the same
//! and binds the exposed `block_hash`.

use eth_light_client_prover::execution::{
    execution_payload_root, native_execution_payload_root, ExecutionPayloadVals,
    EXECUTION_PAYLOAD_GINDEX,
};
use eth_light_client_prover::ssz::{
    load_node, native_merkle_branch_root, verify_merkle_branch, Node,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::utils::testing::base_test;
use halo2_base::utils::ScalarField;
use halo2_base::Context;
use serde_json::Value;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");

fn hexv(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap()
}
fn h32(s: &str) -> [u8; 32] {
    hexv(s).try_into().unwrap()
}
fn u64s(v: &Value) -> u64 {
    v.as_str().unwrap().parse().unwrap()
}

/// uint256 (decimal string) → 32-byte little-endian.
fn u256_le(s: &str) -> [u8; 32] {
    let val: u128 = s.parse().unwrap();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&val.to_le_bytes());
    out
}

fn execution(v: &Value, which: &str) -> ExecutionPayloadVals {
    let e = &v["data"][which]["execution"];
    ExecutionPayloadVals {
        parent_hash: h32(e["parent_hash"].as_str().unwrap()),
        fee_recipient: hexv(e["fee_recipient"].as_str().unwrap()).try_into().unwrap(),
        state_root: h32(e["state_root"].as_str().unwrap()),
        receipts_root: h32(e["receipts_root"].as_str().unwrap()),
        logs_bloom: hexv(e["logs_bloom"].as_str().unwrap()),
        prev_randao: h32(e["prev_randao"].as_str().unwrap()),
        block_number: u64s(&e["block_number"]),
        gas_limit: u64s(&e["gas_limit"]),
        gas_used: u64s(&e["gas_used"]),
        timestamp: u64s(&e["timestamp"]),
        extra_data: hexv(e["extra_data"].as_str().unwrap()),
        base_fee_per_gas: u256_le(e["base_fee_per_gas"].as_str().unwrap()),
        block_hash: h32(e["block_hash"].as_str().unwrap()),
        transactions_root: h32(e["transactions_root"].as_str().unwrap()),
        withdrawals_root: h32(e["withdrawals_root"].as_str().unwrap()),
        blob_gas_used: u64s(&e["blob_gas_used"]),
        excess_blob_gas: u64s(&e["excess_blob_gas"]),
    }
}

fn execution_branch(v: &Value, which: &str) -> Vec<[u8; 32]> {
    v["data"][which]["execution_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect()
}

fn body_root(v: &Value, which: &str) -> [u8; 32] {
    h32(v["data"][which]["beacon"]["body_root"].as_str().unwrap())
}

/// THE validation: exec-payload root + branch @ gindex 25 reconstruct body_root.
#[test]
fn native_execution_branch_reconstructs_body_root() {
    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    let payload = execution(&v, "finalized_header");
    let branch = execution_branch(&v, "finalized_header");
    assert_eq!(branch.len(), 4, "execution_branch depth must be 4 (gindex 25)");

    let leaf = native_execution_payload_root(&payload);
    let recomputed = native_merkle_branch_root(&leaf, &branch, EXECUTION_PAYLOAD_GINDEX);
    assert_eq!(
        recomputed,
        body_root(&v, "finalized_header"),
        "execution branch did not reconstruct body_root — htr/gindex mismatch"
    );

    // attested header too (independent sample).
    let ap = execution(&v, "attested_header");
    let ab = execution_branch(&v, "attested_header");
    let aleaf = native_execution_payload_root(&ap);
    assert_eq!(
        native_merkle_branch_root(&aleaf, &ab, EXECUTION_PAYLOAD_GINDEX),
        body_root(&v, "attested_header"),
    );
}

#[test]
#[ignore = "in-circuit SHA-256; run on n14"]
fn execution_payload_root_and_branch_in_circuit() {
    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    let payload = execution(&v, "finalized_header");
    let branch = execution_branch(&v, "finalized_header");
    let broot = body_root(&v, "finalized_header");
    let native_bh = payload.block_hash;

    base_test().k(20).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let (root, block_hash) = execution_payload_root(&chip, ctx, &payload);
        // bind exposed block_hash
        for (i, &b) in block_hash.iter().enumerate() {
            assert_eq!(b.value().get_lower_32() as u8, native_bh[i], "block_hash byte {i}");
        }
        let branch_nodes: Vec<Node<Fr>> = branch.iter().map(|b| load_node(ctx, b)).collect();
        let root_target = load_node(ctx, &broot);
        // recompute execution root from branch and constrain to body_root
        verify_merkle_branch(&chip, ctx, &root, &branch_nodes, EXECUTION_PAYLOAD_GINDEX, &root_target);
    });
}

/// Regression: the constraint system of `execution_payload_root` must not
/// depend on `extra_data.len()`. The old code padded the 32-byte chunk with
/// `load_constant`, which added `32 - len` constant-equality (copy)
/// constraints and therefore a different VK per finalized block: the fixture
/// VkBlob (27-byte mainnet `extra_data`) rejected a 25-byte Sepolia block.
#[test]
fn execution_root_shape_is_independent_of_extra_data_len() {
    use halo2_base::gates::circuit::{builder::BaseCircuitBuilder, CircuitBuilderStage};

    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    let base = execution(&v, "finalized_header");

    let shape = |extra: Vec<u8>| {
        let mut p = base.clone();
        p.extra_data = extra;
        let native = native_execution_payload_root(&p);
        let mut b = BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Mock)
            .use_k(18)
            .use_lookup_bits(17);
        let range = RangeChip::new(17, b.lookup_manager().clone());
        let chip = Sha256Chip::new(&range);
        let ctx = b.main(0);
        let (root, _) = execution_payload_root(&chip, ctx, &p);
        for (i, &byte) in root.iter().enumerate() {
            assert_eq!(
                byte.value().get_lower_32() as u8,
                native[i],
                "root byte {i}"
            );
        }
        let stats = b.statistics();
        let cm = b.pool(0).copy_manager.lock().unwrap();
        (
            stats.gate.total_advice_per_phase.clone(),
            stats.total_lookup_advice_per_phase.clone(),
            cm.advice_equalities.len(),
            cm.constant_equalities.len(),
        )
    };

    let reference = shape(vec![0xaa; 27]);
    for len in [0usize, 1, 18, 25, 32] {
        let got = shape(vec![0xbb; len]);
        assert_eq!(
            got, reference,
            "circuit shape changed for extra_data len {len}"
        );
    }
}
