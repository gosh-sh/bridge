//! M6 recursive-rotate brick #1 — the committee SHA-root **sharding** is sound.
//!
//! The monolithic `sync_committee_root` (~1023 in-circuit SHA-256) is memory-bound
//! beyond a 125 GB host (see `tests/rotate_mock_prover.rs::full_rotate_in_circuit`).
//! Because `pubkeys_vector_root = merkleize(512 leaves)` is a balanced tree it
//! factors exactly into N contiguous subtrees, so each shard is a small proof and a
//! cheap aggregation finishes the top. These tests pin that decomposition:
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test rotate_shard_mock_prover
//! ```
//!
//! - `shards_reduce_to_full_committee_root` (native): 8×64 subtree roots recompose
//!   byte-for-byte to `native_sync_committee_root`. Also checks live fixture data
//!   when present.
//! - `committee_subtree_root_in_circuit_matches_native` (in-circuit, one 64-pubkey
//!   shard @ k20): the SHA-heavy shard fits comfortably — this is the per-shard
//!   proof the recursion rests on.
//! - `recompose_from_subtrees_in_circuit_matches_native` (in-circuit, cheap @ k18):
//!   the aggregation's cheap top (merkleize N roots + aggregate + container).

use eth_light_client_prover::committee::{
    committee_subtree_root, native_committee_subtree_root, native_sync_committee_root,
    native_sync_committee_root_from_subtrees, sync_committee_root_from_subtrees, COMMITTEE_SHARDS,
    PUBKEYS_PER_SHARD, SYNC_COMMITTEE_SIZE,
};
use eth_light_client_prover::rotate::{
    combine_committee_commitment, commit_committee_shard, commit_sync_committee_2level,
    new_committee_hasher, verify_shard, NEXT_SYNC_COMMITTEE_GINDEX, SHARD_INSTANCE_LEN,
};
use eth_light_client_prover::ssz::{load_bytes, load_node, verify_merkle_branch, Node};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::{RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::dev::MockProver;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::utils::testing::base_test;
use halo2_base::utils::ScalarField;
use halo2_base::{AssignedValue, Context};
use pse_poseidon::Poseidon;
use serde_json::Value;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// Fixtures / synthetic committee (mirrors rotate_mock_prover.rs).
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

fn pk48(s: &str) -> [u8; 48] {
    hex::decode(s.trim_start_matches("0x")).unwrap().try_into().unwrap()
}

fn h32(s: &str) -> [u8; 32] {
    hex::decode(s.trim_start_matches("0x")).unwrap().try_into().unwrap()
}

fn committee_from_fixture(v: &Value) -> Option<(Vec<[u8; 48]>, [u8; 48])> {
    let nsc = &v["next_sync_committee"];
    let pubkeys: Vec<[u8; 48]> =
        nsc["pubkeys"].as_array()?.iter().map(|p| pk48(p.as_str().unwrap())).collect();
    let aggregate = pk48(nsc["aggregate_pubkey"].as_str()?);
    Some((pubkeys, aggregate))
}

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
    (pubkeys, [0xABu8; 48])
}

/// Native: split into COMMITTEE_SHARDS contiguous subtree roots.
fn native_shard_roots(pubkeys: &[[u8; 48]]) -> Vec<[u8; 32]> {
    pubkeys.chunks(PUBKEYS_PER_SHARD).map(native_committee_subtree_root).collect()
}

// --------------------------------------------------------------------------
// 1. The decomposition is exact (native).
// --------------------------------------------------------------------------

#[test]
fn shards_reduce_to_full_committee_root() {
    assert_eq!(SYNC_COMMITTEE_SIZE % COMMITTEE_SHARDS, 0, "shards must divide committee");
    assert!(PUBKEYS_PER_SHARD.is_power_of_two(), "pubkeys-per-shard must be a power of two");
    assert!(COMMITTEE_SHARDS.is_power_of_two(), "shard count must be a power of two");

    let check = |pubkeys: &[[u8; 48]], aggregate: &[u8; 48]| {
        let shard_roots = native_shard_roots(pubkeys);
        assert_eq!(shard_roots.len(), COMMITTEE_SHARDS);
        let recomposed = native_sync_committee_root_from_subtrees(&shard_roots, aggregate);
        let monolithic = native_sync_committee_root(pubkeys, aggregate);
        assert_eq!(recomposed, monolithic, "sharded recomposition != monolithic committee root");
    };

    let (pk, agg) = synthetic_committee();
    check(&pk, &agg);

    if let Some(v) = load_update() {
        if let Some((pk, agg)) = committee_from_fixture(&v) {
            assert_eq!(pk.len(), SYNC_COMMITTEE_SIZE);
            check(&pk, &agg);
            eprintln!("OK: sharding validated on live mainnet committee too");
        }
    } else {
        eprintln!("note: no fixtures/mainnet/update_period_*.json — synthetic-only");
    }
}

// --------------------------------------------------------------------------
// 2. One shard in-circuit (the SHA-heavy proof) matches native.
// --------------------------------------------------------------------------

#[test]
fn committee_subtree_root_in_circuit_matches_native() {
    let (pubkeys, _agg) = synthetic_committee();
    let shard: Vec<[u8; 48]> = pubkeys[0..PUBKEYS_PER_SHARD].to_vec();
    let expected = native_committee_subtree_root(&shard);

    base_test().k(20).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let flat: Vec<u8> = shard.iter().flat_map(|p| p.iter().copied()).collect();
        let pk = load_bytes(ctx, &flat);
        let root = committee_subtree_root(&chip, ctx, &pk);
        let got: Vec<u8> = root.iter().map(|b| b.value().get_lower_32() as u8).collect();
        assert_eq!(got, expected.to_vec(), "in-circuit shard subtree root != native");
    });
}

// --------------------------------------------------------------------------
// 3. The aggregation's cheap top (merkleize shard roots + aggregate + container).
// --------------------------------------------------------------------------

#[test]
fn recompose_from_subtrees_in_circuit_matches_native() {
    let (pubkeys, aggregate) = synthetic_committee();
    let shard_roots = native_shard_roots(&pubkeys);
    let expected = native_sync_committee_root(&pubkeys, &aggregate);

    base_test().k(18).lookup_bits(17).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let roots: Vec<Node<Fr>> = shard_roots.iter().map(|r| load_node(ctx, r)).collect();
        let agg = load_bytes(ctx, &aggregate);
        let root = sync_committee_root_from_subtrees(&chip, ctx, roots, &agg);
        let got: Vec<u8> = root.iter().map(|b| b.value().get_lower_32() as u8).collect();
        assert_eq!(got, expected.to_vec(), "in-circuit recomposition != native full root");
    });
}

// --------------------------------------------------------------------------
// 3b. The FAITHFUL anchor in-circuit over REAL mainnet data: 8 distinct real
//     subtree roots → committee SSZ root → REAL next_sync_committee_branch @
//     gindex 87 → REAL beacon state_root. This is exactly the anchor the N=8
//     recursion-tree root (`examples/rotate_tree_n8.rs`) runs, minus the
//     snark-verification of the shards — so it validates the faithful data path
//     (distinct slices + real branch) locally, without a 40 GB aggregation node.
// --------------------------------------------------------------------------

#[test]
fn next_committee_branch_binds_in_circuit_over_real_shards() {
    let Some(v) = load_update() else {
        eprintln!("SKIP: no fixtures/mainnet/update_period_*.json");
        return;
    };
    let (pubkeys, aggregate) = committee_from_fixture(&v).expect("committee in fixture");
    assert_eq!(pubkeys.len(), SYNC_COMMITTEE_SIZE);

    // 8 DISTINCT real subtree roots (the tree glues exactly these).
    let shard_roots = native_shard_roots(&pubkeys);
    assert_eq!(shard_roots.len(), COMMITTEE_SHARDS);
    assert!(
        shard_roots.windows(2).any(|w| w[0] != w[1]),
        "real shard subtree roots must be distinct (not degenerate copies)"
    );

    let branch: Vec<[u8; 32]> = v["next_sync_committee_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| h32(b.as_str().unwrap()))
        .collect();
    assert_eq!(branch.len(), 6, "next_sync_committee_branch depth must be 6 (gindex 87)");
    let state_root = h32(v["attested_header"]["beacon"]["state_root"].as_str().unwrap());

    base_test().k(20).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let chip = Sha256Chip::new(range);
        let roots: Vec<Node<Fr>> = shard_roots.iter().map(|r| load_node(ctx, r)).collect();
        let agg = load_bytes(ctx, &aggregate);
        let committee_root = sync_committee_root_from_subtrees(&chip, ctx, roots, &agg);
        let branch_nodes: Vec<Node<Fr>> = branch.iter().map(|b| load_node(ctx, b)).collect();
        let sr = load_node(ctx, &state_root);
        // constrains committee_root + branch @ gindex 87 == state_root (fails if data lies).
        verify_merkle_branch(&chip, ctx, &committee_root, &branch_nodes, NEXT_SYNC_COMMITTEE_GINDEX, &sr);
    });
    eprintln!("OK: real 8-shard committee root binds to live state_root via real branch @ gindex 87");
}

// --------------------------------------------------------------------------
// 4. The 2-level Poseidon committee commitment (shard digest + combine).
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

fn sponge(elems: &[Fr]) -> Fr {
    // Same spec as new_committee_hasher (T=3, RATE=2, R_F=8, R_P=57).
    let mut s = Poseidon::<Fr, 3, 2>::new(8, 57);
    s.update(elems);
    s.squeeze()
}

/// Native level-1 shard digest.
fn native_commit_shard(slice: &[[u8; 48]]) -> Fr {
    let mut elems = Vec::with_capacity(slice.len() * 2);
    for pk in slice {
        elems.push(fr_from_le(&pk[0..31]));
        elems.push(fr_from_le(&pk[31..48]));
    }
    sponge(&elems)
}

/// Native full 2-level commitment (twin of `commit_sync_committee_2level`).
fn native_commit_2level(pubkeys: &[[u8; 48]], aggregate: &[u8; 48], per_shard: usize) -> Fr {
    let mut elems: Vec<Fr> = pubkeys.chunks(per_shard).map(native_commit_shard).collect();
    elems.push(fr_from_le(&aggregate[0..31]));
    elems.push(fr_from_le(&aggregate[31..48]));
    sponge(&elems)
}

/// Native single-sponge commitment (v1) — to show the 2-level value differs.
fn native_commit_single(pubkeys: &[[u8; 48]], aggregate: &[u8; 48]) -> Fr {
    let mut elems = Vec::with_capacity(pubkeys.len() * 2 + 2);
    for pk in pubkeys {
        elems.push(fr_from_le(&pk[0..31]));
        elems.push(fr_from_le(&pk[31..48]));
    }
    elems.push(fr_from_le(&aggregate[0..31]));
    elems.push(fr_from_le(&aggregate[31..48]));
    sponge(&elems)
}

#[test]
fn commit_2level_in_circuit_matches_native() {
    let (pubkeys, aggregate) = synthetic_committee();
    let expected = native_commit_2level(&pubkeys, &aggregate, PUBKEYS_PER_SHARD);

    base_test().k(20).lookup_bits(19).run(|ctx: &mut Context<Fr>, range: &RangeChip<Fr>| {
        let gate = range.gate();
        let hasher = new_committee_hasher(ctx, gate);
        let flat: Vec<u8> = pubkeys.iter().flat_map(|p| p.iter().copied()).collect();
        let pk = load_bytes(ctx, &flat);
        let agg = load_bytes(ctx, &aggregate);

        // Full 2-level (step's path).
        let full = commit_sync_committee_2level(&hasher, ctx, gate, &pk, &agg, PUBKEYS_PER_SHARD);
        assert_eq!(*full.value(), expected, "in-circuit 2-level commitment != native");

        // Same value via explicit shard digests + combine (aggregation's path).
        let digests: Vec<AssignedValue<Fr>> = pk
            .chunks(PUBKEYS_PER_SHARD * 48)
            .map(|s| commit_committee_shard(&hasher, ctx, gate, s))
            .collect();
        assert_eq!(digests.len(), COMMITTEE_SHARDS);
        let combined = combine_committee_commitment(&hasher, ctx, gate, &digests, &agg);
        assert_eq!(
            *combined.value(),
            expected,
            "combine(shard_digests) != full 2-level (step vs aggregation paths must agree)"
        );
    });
}

#[test]
fn commit_2level_is_binding_and_distinct_from_single() {
    let (pubkeys, aggregate) = synthetic_committee();
    let base = native_commit_2level(&pubkeys, &aggregate, PUBKEYS_PER_SHARD);

    // A pubkey flip changes it.
    let mut tampered = pubkeys.clone();
    tampered[70][3] ^= 0x01; // shard 1 (indices 64..127)
    assert_ne!(base, native_commit_2level(&tampered, &aggregate, PUBKEYS_PER_SHARD));

    // An aggregate flip changes it.
    let mut agg2 = aggregate;
    agg2[0] ^= 0x01;
    assert_ne!(base, native_commit_2level(&pubkeys, &agg2, PUBKEYS_PER_SHARD));

    // 2-level is a DIFFERENT commitment scheme than the single sponge (v1): the
    // step+rotate re-emit must switch both sides together — they are not mixable.
    assert_ne!(
        base,
        native_commit_single(&pubkeys, &aggregate),
        "2-level must differ from single-sponge (documents the coordinated re-emit)"
    );
}

// --------------------------------------------------------------------------
// 5. The shard INNER circuit (real instance column) — what the aggregation
//    snark-verifies. Instances = [subtree_root_hi, subtree_root_lo, shard_digest].
// --------------------------------------------------------------------------

#[test]
fn shard_circuit_instances_match_native() {
    let (pubkeys, _agg) = synthetic_committee();
    let slice: Vec<[u8; 48]> = pubkeys[0..PUBKEYS_PER_SHARD].to_vec();

    let subtree = native_committee_subtree_root(&slice);
    let expected = vec![
        fr_from_le(&subtree[0..16]),  // subtree_root_hi
        fr_from_le(&subtree[16..32]), // subtree_root_lo
        native_commit_shard(&slice),  // shard_digest
    ];
    assert_eq!(expected.len(), SHARD_INSTANCE_LEN);

    let (k, lb) = (20u32, 19usize);
    let mut builder =
        BaseCircuitBuilder::<Fr>::default().use_k(k as usize).use_lookup_bits(lb).use_instance_columns(1);
    let range = RangeChip::new(lb, builder.lookup_manager().clone());
    let chip = Sha256Chip::new(&range);

    let cells = {
        let ctx = builder.pool(0).main();
        let hasher = new_committee_hasher(ctx, range.gate());
        let flat: Vec<u8> = slice.iter().flat_map(|p| p.iter().copied()).collect();
        let pk = load_bytes(ctx, &flat);
        verify_shard(&chip, &hasher, ctx, range.gate(), &pk)
    };
    let vals: Vec<Fr> = cells.iter().map(|c| *c.value()).collect();
    assert_eq!(vals, expected, "shard circuit instances != native");
    builder.assigned_instances[0] = cells;

    let used = builder.lookup_manager().iter().map(|lm| lm.total_rows()).sum::<usize>();
    builder.config_params.lookup_bits = if used == 0 { None } else { Some(lb) };
    builder.calculate_params(Some(9));

    let prover = MockProver::run(k, &builder, vec![vals]).unwrap();
    prover.assert_satisfied();
}
