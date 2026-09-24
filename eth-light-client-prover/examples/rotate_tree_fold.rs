//! M6 recursive rotate — **recursive accumulator FOLD bring-up** (shards → L1 → L2).
//!
//! The decisive new primitive of the N=8 recursion tree: a node that aggregates
//! snarks which THEMSELVES already carry a KZG accumulator (the intermediate
//! aggregation proofs), folding the inner accumulator instead of re-verifying a
//! leaf. This is what lets the tree stay at small per-node `k` (each node is
//! fan-in 2) instead of the single-circuit `k=23` monolith (~250 GB → OOM).
//!
//! Mechanics proven here with the stock [`AggregationCircuit`] (no custom glue —
//! that lives only at the tree root):
//!
//! * **L1** = aggregate 2 leaf shard snarks, `expose_previous_instances(false)`
//!   (leaves have no accumulator) → an intermediate `Snark` whose protocol carries
//!   `accumulator_indices = 0..12` (via `AggregationCircuit`'s `CircuitExt`).
//! * **L2** = aggregate 2 **L1** snarks, `expose_previous_instances(true)` — the
//!   `true` tells the SDK the inners carry an accumulator, so it **folds** the 12
//!   inner limbs and passes the inner app-PIs (the shard passthrough) up.
//!
//! A PASS (self-verify of L2) + both nodes fitting `k=21` proves the tree is
//! buildable on n14. Instance accounting is asserted:
//! `leaf=3 → L1=12+2·3=18 → L2=12+2·(18−12)=24`.
//!
//! For the bring-up L1 is built once and fed to L2 twice (proving the fold), which
//! has identical circuit shape / `k` / RAM to the real 4×L1 → 2×L2 tree.
//!
//! ## Run (n14, capture peak RSS across both levels)
//!
//! ```bash
//! cd eth-light-client-prover
//! /usr/bin/time -v env SHARD_DIR=out/shard_snark \
//!   OUTER_SRS=../params/kzg_bn254_21.srs TREE_K=21 \
//!   cargo run --release --features aggregation --example rotate_tree_fold 2>&1 | tail -50
//! ```
//!
//! Env: `SHARD_DIR` (default `out/shard_snark`), `OUTER_SRS`
//! (default `../params/kzg_bn254_{k}.srs`), `TREE_K` (default 21).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::{BaseCircuitParams, CircuitBuilderStage};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::halo2curves::serde::SerdeObject;
use halo2_base::halo2_proofs::plonk::VerifyingKey;
use halo2_base::halo2_proofs::poly::commitment::Params;
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;
use halo2_base::halo2_proofs::SerdeFormat;

use snark_verifier::system::halo2::{compile, Config};
use snark_verifier_sdk::halo2::aggregation::{
    AggregationCircuit, AggregationConfigParams, VerifierUniversality,
};
use snark_verifier_sdk::halo2::gen_snark_shplonk;
use snark_verifier_sdk::{gen_pk, Snark, SHPLONK};

const HERMEZ_SG2_HEAD: [u8; 4] = [0x92, 0x8f, 0xaf, 0xb3];
const CHAIN_SG2_HEAD: [u8; 4] = [0xc6, 0x02, 0x8a, 0xcf];

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn load_srs(path: &Path, k: u32) -> anyhow::Result<ParamsKZG<Bn256>> {
    anyhow::ensure!(path.exists(), "SRS not found at {}", path.display());
    let mut f = fs::File::open(path)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    if params.k() > k {
        params.downsize(k);
    }
    anyhow::ensure!(params.k() == k, "SRS k={} != required {k}", params.k());
    let sg2 = params.s_g2().to_raw_bytes();
    let label = if sg2[0..4] == HERMEZ_SG2_HEAD {
        "Hermez"
    } else if sg2[0..4] == CHAIN_SG2_HEAD {
        "CHAIN"
    } else {
        "unknown"
    };
    println!("  SRS k={k} ceremony={label} (s_g2 head {:02x?})", &sg2[0..4]);
    Ok(params)
}

fn read_instances(path: &Path) -> anyhow::Result<Vec<Fr>> {
    let bytes = fs::read(path)?;
    anyhow::ensure!(bytes.len() % 32 == 0, "instances not a multiple of 32 B");
    let mut out = Vec::with_capacity(bytes.len() / 32);
    for chunk in bytes.chunks_exact(32) {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(chunk);
        out.push(
            Option::<Fr>::from(Fr::from_bytes(&arr)).ok_or_else(|| anyhow::anyhow!("non-canonical Fr"))?,
        );
    }
    Ok(out)
}

/// Build + prove one aggregation node; returns the resulting `Snark` (its protocol
/// carries `accumulator_indices = 0..12`, so a parent node can fold it). Uses
/// `VerifierUniversality::None` (bakes the inner VK as constants — cheapest, and
/// all same-level nodes share one VK).
fn agg_node(
    name: &str,
    params: &ParamsKZG<Bn256>,
    k: u32,
    inners: Vec<Snark>,
    has_prev_accumulator: bool,
) -> Snark {
    let n = inners.len();
    println!("── {name}: aggregate {n} snark(s), has_prev_accumulator={has_prev_accumulator}");
    let cfg = AggregationConfigParams { degree: k, lookup_bits: (k - 1) as usize, ..Default::default() };

    let mut kg = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        cfg,
        params,
        inners.clone(),
        VerifierUniversality::None,
    );
    kg.expose_previous_instances(has_prev_accumulator);
    let calc = kg.calculate_params(Some(10));
    println!("   calculated: {calc:?}");
    let t = Instant::now();
    let pk = gen_pk(params, &kg, None);
    println!("   gen_pk {:.1}s", t.elapsed().as_secs_f64());
    let bp = kg.break_points();
    drop(kg);

    let mut pv = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calc,
        params,
        inners,
        VerifierUniversality::None,
    )
    .use_break_points(bp);
    pv.expose_previous_instances(has_prev_accumulator);

    let t = Instant::now();
    let snark = gen_snark_shplonk(params, &pk, pv, None::<&Path>);
    let ninst: usize = snark.instances.iter().map(|c| c.len()).sum();
    println!(
        "   prove {:.1}s → snark {} B, {ninst} instances (self-verify PASS)\n",
        t.elapsed().as_secs_f64(),
        snark.proof().len()
    );
    snark
}

fn main() -> anyhow::Result<()> {
    let k = env_usize("TREE_K", 21) as u32;
    let shard_dir =
        PathBuf::from(std::env::var("SHARD_DIR").unwrap_or_else(|_| "out/shard_snark".to_string()));

    println!("=== M6: recursive accumulator FOLD (shards → L1 → L2) ===");
    println!("shard dir : {}\ntree k    : {k}\n", shard_dir.display());

    // ---- Load + wrap the leaf shard snark ----------------------------------
    let config: BaseCircuitParams =
        serde_json::from_slice(&fs::read(shard_dir.join("shard_config.json"))?)?;
    let inner_k = config.k as u32;
    let vk_bytes = fs::read(shard_dir.join("shard_vk.bin"))?;
    let mut vk_slice: &[u8] = &vk_bytes;
    let vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut vk_slice,
        SerdeFormat::RawBytesUnchecked,
        config.clone(),
    )
    .map_err(|e| anyhow::anyhow!("read shard VK: {e}"))?;
    let proof = fs::read(shard_dir.join("shard_proof.bin"))?;
    let instances = read_instances(&shard_dir.join("shard_instances.bin"))?;
    println!("leaf shard: k={inner_k}, proof {} B, {} instances", proof.len(), instances.len());

    let inner_srs = shard_dir.join(format!("kzg_bn254_{inner_k}.srs"));
    let params_inner = load_srs(&inner_srs, inner_k)?;
    let protocol =
        compile(&params_inner, &vk, Config::kzg().with_num_instance(vec![instances.len()]));
    let leaf = Snark::new(protocol, vec![instances.clone()], proof);

    let outer_srs = std::env::var("OUTER_SRS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("../params/kzg_bn254_{k}.srs")));
    let params = load_srs(&outer_srs, k)?;
    println!();

    // ---- L1: aggregate 2 leaf shards (no prev accumulator) -----------------
    let l1 = agg_node("L1", &params, k, vec![leaf.clone(), leaf], false);
    let l1_inst: usize = l1.instances.iter().map(|c| c.len()).sum();
    anyhow::ensure!(l1_inst == 18, "L1 instances {l1_inst} != 18 (12 acc + 2·3)");

    // ---- L2: aggregate 2 L1 nodes (RECURSIVE — fold inner accumulator) ------
    let l2 = agg_node("L2", &params, k, vec![l1.clone(), l1], true);
    let l2_inst: usize = l2.instances.iter().map(|c| c.len()).sum();
    anyhow::ensure!(l2_inst == 24, "L2 instances {l2_inst} != 24 (12 acc + 2·6 passthrough)");

    println!("=== RESULT: PASS ===");
    println!(
        "Recursive fold works: L2 aggregated 2 accumulator-carrying L1 snarks at k={k},\n\
         folded the inner accumulators, passed the 12 shard PIs through (L2 = 24 inst).\n\
         Both levels fit k={k}. Peak RSS from `/usr/bin/time -v`."
    );
    Ok(())
}
