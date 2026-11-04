//! `key_cache`: a stored key reads back as the same key for the build that
//! wrote it, and as no key for any other build or when it is incomplete.

use std::path::PathBuf;

use eth_light_client_prover::key_cache::KeyCache;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::{BaseCircuitParams, CircuitBuilderStage};
use halo2_base::gates::{GateChip, GateInstructions};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk, ProvingKey};
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;
use halo2_base::halo2_proofs::SerdeFormat;
use rand::rngs::OsRng;

const K: u32 = 8;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-key-cache-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn keygen() -> (BaseCircuitParams, Vec<Vec<usize>>, ProvingKey<G1Affine>) {
    let params = ParamsKZG::<Bn256>::setup(K, OsRng);
    let mut b = BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
        .use_k(K as usize)
        .use_instance_columns(1);
    let gate = GateChip::<Fr>::default();
    let ctx = b.main(0);
    let x = ctx.load_witness(Fr::from(7u64));
    let y = ctx.load_witness(Fr::from(11u64));
    let z = gate.mul(ctx, x, y);
    b.assigned_instances[0] = vec![z];
    b.config_params.lookup_bits = None;
    let config = b.calculate_params(Some(9));
    let vk = keygen_vk(&params, &b).unwrap();
    let pk = keygen_pk(&params, vk, &b).unwrap();
    (config, b.break_points(), pk)
}

fn vk_bytes(pk: &ProvingKey<G1Affine>) -> Vec<u8> {
    let mut out = Vec::new();
    pk.get_vk().write(&mut out, SerdeFormat::RawBytes).unwrap();
    out
}

#[test]
fn a_stored_key_reads_back_for_its_build_only() {
    let dir = scratch("roundtrip");
    let (config, break_points, pk) = keygen();
    let cache = KeyCache::new(&dir, "build-a");
    assert!(cache
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("shard", |p| p.clone())
        .unwrap()
        .is_none());

    cache.store("shard", &config, &break_points, &pk).unwrap();
    let stored = cache
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("shard", |p| p.clone())
        .unwrap()
        .expect("the key this build stored");
    assert_eq!(stored.params.k, config.k);
    assert_eq!(stored.break_points, break_points);
    assert_eq!(vk_bytes(&stored.pk), vk_bytes(&pk));

    let other = KeyCache::new(&dir, "build-b");
    assert!(other
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("shard", |p| p.clone())
        .unwrap()
        .is_none());

    // The other build regenerates and overwrites; the first build's key is gone.
    other.store("shard", &config, &break_points, &pk).unwrap();
    assert!(other
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("shard", |p| p.clone())
        .unwrap()
        .is_some());
    assert!(cache
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("shard", |p| p.clone())
        .unwrap()
        .is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_key_without_its_proving_key_file_is_no_key() {
    let dir = scratch("partial");
    let (config, break_points, pk) = keygen();
    let cache = KeyCache::new(&dir, "build-a");
    cache.store("tree-l1", &config, &break_points, &pk).unwrap();
    std::fs::remove_file(dir.join("tree-l1/pk.bin")).unwrap();
    std::fs::write(dir.join("tree-l1/pk.bin.tmp"), b"half a key").unwrap();
    assert!(cache
        .load::<BaseCircuitBuilder<Fr>, BaseCircuitParams>("tree-l1", |p| p.clone())
        .unwrap()
        .is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}
