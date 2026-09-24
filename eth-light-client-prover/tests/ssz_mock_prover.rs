//! M2 SSZ tests against the real mainnet `finality_update` fixture (fork fulu).
//!
//! Off-circuit tests (always run) validate the fixture + the M0 gindex table.
//! MockProver tests (`#[ignore]`, run on n14) prove the **in-circuit** SSZ output
//! equals the native reference on real data:
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test ssz_mock_prover -- --ignored --nocapture
//! ```
//!
//! The full 512-pubkey `sync_committee_root` is **not** MockProver-tested here:
//! it is ~1023 in-circuit SHA-256 (~k26), a rotation-only cost handled by a
//! separate "rotate" proof in M3 (see `docs/m2_notes.md`). Its building blocks
//! (`bytes_root` on a 48-byte pubkey, small `merkleize`) are proven instead.

use eth_light_client_prover::signing::{
    beacon_header_root, native_beacon_header_root, native_sync_committee_signing_root,
    sync_committee_signing_root, FORK_VERSION_FULU, MAINNET_GENESIS_VALIDATORS_ROOT,
};
use eth_light_client_prover::ssz::{
    bytes_root, load_bytes, load_node, merkleize, native_bytes_root, native_merkle_branch_root,
    native_merkleize, verify_merkle_branch, Node,
};
use eth_light_client_prover::committee::native_sync_committee_root;
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::utils::testing::base_test;
use halo2_base::utils::ScalarField;
use halo2_base::{AssignedValue, Context};
use serde_json::Value;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");

// Electra/Fulu generalized indices (M0 spec §3).
const FINALIZED_ROOT_GINDEX: u64 = 169;

// ---------------------------------------------------------------------------
// Fixture parsing helpers
// ---------------------------------------------------------------------------

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn h32(s: &str) -> [u8; 32] {
    let bytes = hex::decode(s.trim_start_matches("0x")).unwrap();
    bytes.try_into().unwrap()
}

struct Header {
    slot: u64,
    proposer_index: u64,
    parent_root: [u8; 32],
    state_root: [u8; 32],
    body_root: [u8; 32],
}

fn header(v: &Value, which: &str) -> Header {
    let b = &v["data"][which]["beacon"];
    Header {
        slot: b["slot"].as_str().unwrap().parse().unwrap(),
        proposer_index: b["proposer_index"].as_str().unwrap().parse().unwrap(),
        parent_root: h32(b["parent_root"].as_str().unwrap()),
        state_root: h32(b["state_root"].as_str().unwrap()),
        body_root: h32(b["body_root"].as_str().unwrap()),
    }
}

fn finality_branch(v: &Value) -> Vec<[u8; 32]> {
    v["data"]["finality_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect()
}

fn load_header<F: halo2_base::utils::BigPrimeField>(
    ctx: &mut Context<F>,
    h: &Header,
) -> (Vec<AssignedValue<F>>, Vec<AssignedValue<F>>, Node<F>, Node<F>, Node<F>) {
    let slot = load_bytes(ctx, &h.slot.to_le_bytes());
    let proposer = load_bytes(ctx, &h.proposer_index.to_le_bytes());
    let parent = load_node(ctx, &h.parent_root);
    let state = load_node(ctx, &h.state_root);
    let body = load_node(ctx, &h.body_root);
    (slot, proposer, parent, state, body)
}

// ---------------------------------------------------------------------------
// Off-circuit — validate the fixture and the gindex table
// ---------------------------------------------------------------------------

/// THE key check: the native finalized-header root + `finality_branch` at gindex
/// 169 must reconstruct the attested header's `state_root`. If this passes, the
/// M0 Electra/Fulu gindex is correct on real data and our header htr is right.
#[test]
fn native_finality_branch_reconstructs_attested_state_root() {
    let v = fixture();
    let attested = header(&v, "attested_header");
    let finalized = header(&v, "finalized_header");
    let branch = finality_branch(&v);
    assert_eq!(branch.len(), 7, "finality_branch depth (fulu) must be 7");

    let leaf = native_beacon_header_root(
        finalized.slot,
        finalized.proposer_index,
        &finalized.parent_root,
        &finalized.state_root,
        &finalized.body_root,
    );
    let recomputed = native_merkle_branch_root(&leaf, &branch, FINALIZED_ROOT_GINDEX);
    assert_eq!(
        recomputed, attested.state_root,
        "finality branch did not reconstruct attested state_root — gindex/htr mismatch"
    );
}

#[test]
fn native_signing_root_pipeline_runs() {
    let v = fixture();
    let attested = header(&v, "attested_header");
    let sr = native_sync_committee_signing_root(
        attested.slot,
        attested.proposer_index,
        &attested.parent_root,
        &attested.state_root,
        &attested.body_root,
        &FORK_VERSION_FULU,
        &MAINNET_GENESIS_VALIDATORS_ROOT,
    );
    assert_ne!(sr, [0u8; 32]);
}

#[test]
fn native_sync_committee_root_512_deterministic() {
    let pubkeys = vec![[0x11u8; 48]; 512];
    let agg = [0x22u8; 48];
    let r1 = native_sync_committee_root(&pubkeys, &agg);
    let r2 = native_sync_committee_root(&pubkeys, &agg);
    assert_eq!(r1, r2);
    assert_ne!(r1, [0u8; 32]);
}

// ---------------------------------------------------------------------------
// MockProver — in-circuit == native on real fixture data
// ---------------------------------------------------------------------------

#[test]
#[ignore = "in-circuit SHA-256; run on n14"]
fn header_root_matches_native() {
    let v = fixture();
    let h = header(&v, "attested_header");
    let native = native_beacon_header_root(
        h.slot,
        h.proposer_index,
        &h.parent_root,
        &h.state_root,
        &h.body_root,
    );

    base_test().k(19).lookup_bits(18).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let (slot, proposer, parent, state, body) = load_header(ctx, &h);
        let root = beacon_header_root(&chip, ctx, &slot, &proposer, &parent, &state, &body);
        for (i, &b) in root.iter().enumerate() {
            assert_eq!(b.value().get_lower_32() as u8, native[i], "byte {i}");
        }
    });
}

#[test]
#[ignore = "in-circuit SHA-256; run on n14"]
fn signing_root_matches_native() {
    let v = fixture();
    let h = header(&v, "attested_header");
    let native = native_sync_committee_signing_root(
        h.slot,
        h.proposer_index,
        &h.parent_root,
        &h.state_root,
        &h.body_root,
        &FORK_VERSION_FULU,
        &MAINNET_GENESIS_VALIDATORS_ROOT,
    );

    base_test().k(19).lookup_bits(18).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let (slot, proposer, parent, state, body) = load_header(ctx, &h);
        let fork = load_bytes(ctx, &FORK_VERSION_FULU);
        let gvr = load_node(ctx, &MAINNET_GENESIS_VALIDATORS_ROOT);
        let sr = sync_committee_signing_root(
            &chip, ctx, &slot, &proposer, &parent, &state, &body, &fork, &gvr,
        );
        for (i, &b) in sr.iter().enumerate() {
            assert_eq!(b.value().get_lower_32() as u8, native[i], "byte {i}");
        }
    });
}

#[test]
#[ignore = "in-circuit SHA-256; run on n14"]
fn finality_branch_verifies_in_circuit() {
    let v = fixture();
    let attested = header(&v, "attested_header");
    let finalized = header(&v, "finalized_header");
    let branch = finality_branch(&v);

    base_test().k(20).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let (slot, proposer, parent, state, body) = load_header(ctx, &finalized);
        let leaf = beacon_header_root(&chip, ctx, &slot, &proposer, &parent, &state, &body);
        let branch_nodes: Vec<Node<Fr>> = branch.iter().map(|b| load_node(ctx, b)).collect();
        let root = load_node(ctx, &attested.state_root);
        // Constrains recomputed root == attested state_root; MockProver fails if wrong.
        verify_merkle_branch(&chip, ctx, &leaf, &branch_nodes, FINALIZED_ROOT_GINDEX, &root);
    });
}

#[test]
#[ignore = "in-circuit SHA-256; run on n14"]
fn bytes_root_and_merkleize_building_blocks() {
    // Validate the SSZ building blocks used by sync_committee_root, cheaply.
    let pk = [0xABu8; 48];
    let native_pk_root = native_bytes_root(&pk);
    let leaves: Vec<[u8; 32]> = (0..4).map(|i| [i as u8; 32]).collect();
    let native_merkle = native_merkleize(leaves.clone());

    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let pk_assigned = load_bytes(ctx, &pk);
        let pk_root = bytes_root(&chip, ctx, &pk_assigned);
        for (i, &b) in pk_root.iter().enumerate() {
            assert_eq!(b.value().get_lower_32() as u8, native_pk_root[i], "pk byte {i}");
        }
        let leaf_nodes: Vec<Node<Fr>> = leaves.iter().map(|l| load_node(ctx, l)).collect();
        let m = merkleize(&chip, ctx, leaf_nodes);
        for (i, &b) in m.iter().enumerate() {
            assert_eq!(b.value().get_lower_32() as u8, native_merkle[i], "merkle byte {i}");
        }
    });
}
