//! M3-rotate tests: committee ↔ anchor binding + Poseidon commitment.
//!
//! ```bash
//! cd eth-light-client-prover
//! scripts/fetch_lc_fixtures.sh mainnet          # writes fixtures/mainnet/update_period_*.json
//! cargo test --test rotate_mock_prover                       # off-circuit + cheap in-circuit
//! cargo test --test rotate_mock_prover -- --ignored --nocapture   # + heavy full rotate (n14)
//! ```
//!
//! - `native_next_committee_branch_reconstructs_state_root`: the anchor — SSZ
//!   `htr(SyncCommittee)` + `next_sync_committee_branch` @ gindex 87 reconstruct
//!   the attested `state_root` on **live mainnet update data**.
//! - `commit_sync_committee_matches_native`: in-circuit Poseidon commitment ==
//!   native `pse_poseidon` sponge.
//! - `commit_sync_committee_is_binding`: flipping one pubkey byte changes it.
//! - `full_rotate_in_circuit` (#[ignore]): the complete `verify_rotate` (SSZ root
//!   + branch + commitment) on live data — heavy (~k26).

use eth_light_client_prover::committee::{native_sync_committee_root, SYNC_COMMITTEE_SIZE};
use eth_light_client_prover::rotate::{
    commit_sync_committee, new_committee_hasher, verify_rotate, NEXT_SYNC_COMMITTEE_GINDEX,
};
use eth_light_client_prover::ssz::{load_bytes, load_node, native_merkle_branch_root, Node};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::{RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::utils::testing::base_test;
use halo2_base::AssignedValue;
use halo2_base::Context;
use pse_poseidon::Poseidon;
use serde_json::Value;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// Fixture loading (update_period_*.json is gitignored — read at runtime so the
// test target compiles without it; tests skip gracefully when absent).
// --------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/mainnet")
}

fn load_update() -> Option<Value> {
    let dir = fixture_dir();
    let entry = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).find(|e| {
        e.file_name().to_string_lossy().starts_with("update_period_")
            && e.file_name().to_string_lossy().ends_with(".json")
    })?;
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
    let data = if raw.is_array() { raw[0]["data"].clone() } else { raw["data"].clone() };
    Some(data)
}

fn hexv(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap()
}
fn h32(s: &str) -> [u8; 32] {
    hexv(s).try_into().unwrap()
}
fn pk48(s: &str) -> [u8; 48] {
    hexv(s).try_into().unwrap()
}

fn committee(v: &Value) -> (Vec<[u8; 48]>, [u8; 48]) {
    let nsc = &v["next_sync_committee"];
    let pubkeys: Vec<[u8; 48]> =
        nsc["pubkeys"].as_array().unwrap().iter().map(|p| pk48(p.as_str().unwrap())).collect();
    let aggregate = pk48(nsc["aggregate_pubkey"].as_str().unwrap());
    (pubkeys, aggregate)
}

// --------------------------------------------------------------------------
// Native Poseidon twin (must match in-circuit hash_fix_len_array).
// --------------------------------------------------------------------------

fn fr_from_le(bytes: &[u8]) -> Fr {
    let mut acc = Fr::from(0u64);
    let mut base = Fr::from(1u64);
    let byte = Fr::from(256u64);
    for &b in bytes {
        acc += Fr::from(b as u64) * base;
        base *= byte;
    }
    acc
}

fn native_commit(pubkeys: &[[u8; 48]], aggregate: &[u8; 48]) -> Fr {
    let mut elems = Vec::with_capacity(pubkeys.len() * 2 + 2);
    for pk in pubkeys {
        elems.push(fr_from_le(&pk[0..31]));
        elems.push(fr_from_le(&pk[31..48]));
    }
    elems.push(fr_from_le(&aggregate[0..31]));
    elems.push(fr_from_le(&aggregate[31..48]));
    // Same spec as new_committee_hasher (T=3, RATE=2, R_F=8, R_P=57).
    let mut sponge = Poseidon::<Fr, 3, 2>::new(8, 57);
    sponge.update(&elems);
    sponge.squeeze()
}

/// Deterministic synthetic committee (no fixture needed) for commitment tests.
fn synthetic_committee() -> (Vec<[u8; 48]>, [u8; 48]) {
    let pubkeys: Vec<[u8; 48]> = (0..SYNC_COMMITTEE_SIZE)
        .map(|i| {
            let mut pk = [0u8; 48];
            for (j, b) in pk.iter_mut().enumerate() {
                *b = ((i * 7 + j * 13 + 1) % 251) as u8;
            }
            pk
        })
        .collect();
    let aggregate = [0xABu8; 48];
    (pubkeys, aggregate)
}

fn load_flat<F: halo2_base::utils::BigPrimeField>(
    ctx: &mut Context<F>,
    pubkeys: &[[u8; 48]],
    aggregate: &[u8; 48],
) -> (Vec<AssignedValue<F>>, Vec<AssignedValue<F>>) {
    let flat: Vec<u8> = pubkeys.iter().flat_map(|p| p.iter().copied()).collect();
    (load_bytes(ctx, &flat), load_bytes(ctx, aggregate))
}

// --------------------------------------------------------------------------
// The anchor (off-circuit, live data).
// --------------------------------------------------------------------------

#[test]
fn native_next_committee_branch_reconstructs_state_root() {
    let Some(v) = load_update() else {
        eprintln!("SKIP: no fixtures/mainnet/update_period_*.json (run scripts/fetch_lc_fixtures.sh)");
        return;
    };
    let (pubkeys, aggregate) = committee(&v);
    assert_eq!(pubkeys.len(), SYNC_COMMITTEE_SIZE);

    let branch: Vec<[u8; 32]> = v["next_sync_committee_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| h32(b.as_str().unwrap()))
        .collect();
    assert_eq!(branch.len(), 6, "next_sync_committee_branch depth must be 6 (Electra gindex 87)");

    let scr = native_sync_committee_root(&pubkeys, &aggregate);
    let recomputed = native_merkle_branch_root(&scr, &branch, NEXT_SYNC_COMMITTEE_GINDEX);
    let state_root = h32(v["attested_header"]["beacon"]["state_root"].as_str().unwrap());
    assert_eq!(
        recomputed, state_root,
        "committee root + branch @ gindex {NEXT_SYNC_COMMITTEE_GINDEX} did not reconstruct state_root"
    );
}

// --------------------------------------------------------------------------
// Poseidon commitment (cheap in-circuit).
// --------------------------------------------------------------------------

#[test]
fn commit_sync_committee_matches_native() {
    let (pubkeys, aggregate) = synthetic_committee();
    let expected = native_commit(&pubkeys, &aggregate);

    base_test().k(20).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let hasher = new_committee_hasher(ctx, range.gate());
        let (pk, agg) = load_flat(ctx, &pubkeys, &aggregate);
        let commit = commit_sync_committee(&hasher, ctx, range.gate(), &pk, &agg);
        assert_eq!(*commit.value(), expected, "in-circuit commitment != native pse_poseidon");
    });
}

#[test]
fn commit_sync_committee_is_binding() {
    let (pubkeys, aggregate) = synthetic_committee();
    let base = native_commit(&pubkeys, &aggregate);

    let mut tampered = pubkeys.clone();
    tampered[3][7] ^= 0x01;
    assert_ne!(base, native_commit(&tampered, &aggregate), "flip in pubkey must change commitment");

    let mut agg2 = aggregate;
    agg2[0] ^= 0x01;
    assert_ne!(base, native_commit(&pubkeys, &agg2), "flip in aggregate must change commitment");
}

// --------------------------------------------------------------------------
// Full rotate (heavy in-circuit, live data).
// --------------------------------------------------------------------------

#[test]
#[ignore = "full SSZ committee root (~1023 SHA-256) is memory-bound: the assignment \
            (not 2^k padding) exceeds a 125 GB host at k25/k26 (thrashes). Run on a \
            larger-RAM box. The pieces are covered elsewhere: SSZ primitives by M2 \
            in-circuit tests, committee-root+branch natively on live data \
            (native_next_committee_branch_reconstructs_state_root), Poseidon \
            commitment in-circuit (commit_sync_committee_matches_native)."]
fn full_rotate_in_circuit() {
    let Some(v) = load_update() else {
        eprintln!("SKIP: no update fixture");
        return;
    };
    let (pubkeys, aggregate) = committee(&v);
    let branch: Vec<[u8; 32]> = v["next_sync_committee_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| h32(b.as_str().unwrap()))
        .collect();
    let state_root = h32(v["attested_header"]["beacon"]["state_root"].as_str().unwrap());
    let expected_commit = native_commit(&pubkeys, &aggregate);

    base_test().k(26).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let hasher = new_committee_hasher(ctx, range.gate());
        let (pk, agg) = load_flat(ctx, &pubkeys, &aggregate);
        let branch_nodes: Vec<Node<Fr>> = branch.iter().map(|b| load_node(ctx, b)).collect();
        let sr = load_node(ctx, &state_root);
        let commit = verify_rotate(
            &chip,
            &hasher,
            ctx,
            range.gate(),
            &pk,
            &agg,
            &branch_nodes,
            NEXT_SYNC_COMMITTEE_GINDEX,
            &sr,
        );
        assert_eq!(*commit.value(), expected_commit, "rotate commitment != native");
    });
}
