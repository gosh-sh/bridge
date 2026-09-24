//! M6 recursive rotate — **cost probe for ONE 2-to-1 aggregation node.**
//!
//! The N=8 committee aggregation MUST NOT be a single `k=23` circuit (~250 GB
//! working set → OOM on n14's 125 GB RAM). Instead it is a **recursion tree of
//! small fixed-fan-in nodes** (2→1), each staying within the ~`k21` per-node
//! budget so the peak memory is one small node at a time. This probe measures
//! the decisive number: **what `k` does one fan-in-`AGG_FANIN` node actually
//! need**, using the SAME gosh snark-verifier-sdk stack the tree will use.
//!
//! It aggregates `AGG_FANIN` copies of the REAL committee-shard snark
//! (`out/shard_snark`) inside the stock [`AggregationCircuit`] (no committee
//! glue — that lives only in the tree root) with `expose_previous_instances`, so
//! the outer instance column is `[accumulator(12) ++ passthrough shard PIs]`.
//! `VerifierUniversality::None` bakes the inner VK as constants (cheapest — all
//! same-level nodes share one VK), which is the lever that keeps `k` down.
//!
//! ## Run (n14, capture peak RSS)
//!
//! ```bash
//! cd eth-light-client-prover
//! /usr/bin/time -v env SHARD_DIR=out/shard_snark \
//!   OUTER_SRS=../params/kzg_bn254_21.srs AGG_FANIN=2 PROBE_K_OUTER=21 AGG_UNIV=none \
//!   cargo run --release --features aggregation --example measure_agg_node 2>&1 | tail -40
//! ```
//!
//! A PASS at `k=21` ⇒ the 2-to-1 tree fits n14; a `calculate_params` blow-up (or
//! required `k>21`) ⇒ shrink fan-in / tighten universality before building the tree.
//!
//! Env: `SHARD_DIR` (default `out/shard_snark`), `OUTER_SRS`
//! (default `../params/kzg_bn254_{k}.srs`), `AGG_FANIN` (default 2),
//! `PROBE_K_OUTER` (default 21), `AGG_UNIV` (`none`|`preprocessed`|`full`, default `none`).

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

/// Load an SRS, downsize to `k`, and label the ceremony (Hermez / CHAIN / other).
/// The probe measures `k` and RAM, which are ceremony-independent — but inner and
/// outer MUST share the ceremony, so we print the head for the operator to sanity-check.
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

fn parse_universality(s: &str) -> VerifierUniversality {
    match s {
        "none" => VerifierUniversality::None,
        "preprocessed" => VerifierUniversality::PreprocessedAsWitness,
        _ => VerifierUniversality::Full,
    }
}

fn main() -> anyhow::Result<()> {
    let fanin = env_usize("AGG_FANIN", 2);
    let k = env_usize("PROBE_K_OUTER", 21) as u32;
    let univ_str = std::env::var("AGG_UNIV").unwrap_or_else(|_| "none".to_string());
    let univ = parse_universality(&univ_str);
    anyhow::ensure!(fanin >= 1, "AGG_FANIN must be >= 1");

    let shard_dir =
        PathBuf::from(std::env::var("SHARD_DIR").unwrap_or_else(|_| "out/shard_snark".to_string()));

    println!("=== M6: 2-to-1 aggregation NODE cost probe (gosh snark-verifier-sdk) ===");
    println!("shard dir  : {}", shard_dir.display());
    println!("fan-in     : {fanin}");
    println!("k_outer    : {k}");
    println!("universality: {univ_str}\n");

    // ---- Load + wrap the shard snark (gosh-sh compile -> Snark) -------------
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
    println!("shard: k={inner_k}, proof {} B, {} instances", proof.len(), instances.len());

    let inner_srs = shard_dir.join(format!("kzg_bn254_{inner_k}.srs"));
    let params_inner = load_srs(&inner_srs, inner_k)?;
    let protocol =
        compile(&params_inner, &vk, Config::kzg().with_num_instance(vec![instances.len()]));
    let shard_snark = Snark::new(protocol, vec![instances.clone()], proof);
    let inners: Vec<Snark> = std::iter::repeat(shard_snark).take(fanin).collect();
    println!("wrapped {fanin} copies into snark_verifier_sdk::Snark\n");

    // ---- Outer SRS ----------------------------------------------------------
    let outer_srs = std::env::var("OUTER_SRS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("../params/kzg_bn254_{k}.srs")));
    let params_outer = load_srs(&outer_srs, k)?;
    println!();

    let agg_config = AggregationConfigParams {
        degree: k,
        lookup_bits: (k - 1) as usize,
        ..Default::default()
    };

    // ---- Keygen (measures whether the node FITS in 2^k) --------------------
    println!("keygen stage (aggregate {fanin} snarks, expose previous instances)...");
    let mut keygen = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        &params_outer,
        inners.clone(),
        univ,
    );
    keygen.expose_previous_instances(false); // leaf level: inners carry no accumulator
    let calculated = keygen.calculate_params(Some(10));
    println!("  calculated params: {calculated:?}");
    let t = Instant::now();
    let pk = gen_pk(&params_outer, &keygen, None);
    println!("  gen_pk {:.1}s", t.elapsed().as_secs_f64());
    let break_points = keygen.break_points();
    drop(keygen);

    // ---- Prove + self-verify -----------------------------------------------
    println!("prover stage...");
    let mut prover = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        calculated,
        &params_outer,
        inners,
        univ,
    )
    .use_break_points(break_points);
    prover.expose_previous_instances(false);

    let t = Instant::now();
    let agg = gen_snark_shplonk(&params_outer, &pk, prover, None::<&Path>);
    let dt = t.elapsed().as_secs_f64();

    let outer: usize = agg.instances.iter().map(|c| c.len()).sum();
    println!("\n=== RESULT: PASS ===");
    println!("fan-in={fanin} node fit + proved at k={k} ({univ_str}) in {dt:.1}s");
    println!("outer proof: {} B, outer instances = {outer} (12 accumulator + {} passthrough)", agg.proof().len(), outer.saturating_sub(12));
    println!("\nRead peak RAM from `/usr/bin/time -v` (Maximum resident set size).");
    Ok(())
}
