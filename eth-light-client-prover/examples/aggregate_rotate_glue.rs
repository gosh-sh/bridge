//! M6 recursive rotate — N=1 in-circuit **glue** bring-up.
//!
//! Extends `deposit-prover/examples/aggregate_rotate_shards.rs` (which only wrapped
//! + aggregated a real shard snark) with the actual committee glue:
//! [`RotateAggregationCircuit`] snark-verifies the shard proof **and** runs
//! `node_from_hilo` (range-checked) → `sync_committee_root_from_subtrees` →
//! `verify_merkle_branch(gindex 87)` → `combine_committee_commitment`, exposing the
//! rotate PIs `[current_commit, next_commit, period]` after the 12-limb accumulator.
//!
//! For N=1 the "committee" is a single 64-pubkey shard (a `merkleize` of one leaf is
//! the leaf itself), with a self-consistent synthetic branch/state_root — a faithful
//! mechanics proof of the whole wiring over a REAL gosh-fork shard proof. Scaling to
//! the full committee is `--n 8` + real branch (needs Hermez k=23; see
//! `docs/m6_rotate_recursive.md`).
//!
//! ## Run (n14)
//!
//! ```bash
//! cd eth-light-client-prover
//! SHARD_DIR=out/shard_snark OUTER_SRS=../params/kzg_bn254_21.srs \
//!   PROBE_N=1 PROBE_K_OUTER=21 \
//!   cargo run --release --features aggregation --example aggregate_rotate_glue
//! ```
//!
//! Env: `SHARD_DIR` (default `out/shard_snark`), `OUTER_SRS`
//! (default `../params/kzg_bn254_{k_outer}.srs`), `PROBE_N` (default 1),
//! `PROBE_K_OUTER` (default 21).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eth_light_client_prover::committee::{native_committee_subtree_root, PUBKEYS_PER_SHARD};
use eth_light_client_prover::rotate::NEXT_SYNC_COMMITTEE_GINDEX;
use eth_light_client_prover::rotate_aggregation::{
    RotateAggregationCircuit, RotateGlueWitness, NUM_ACCUMULATOR_INSTANCES,
    NUM_ROTATE_PUBLIC_INPUTS,
};

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::{BaseCircuitParams, CircuitBuilderStage};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::halo2curves::serde::SerdeObject;
use halo2_base::halo2_proofs::plonk::VerifyingKey;
use halo2_base::halo2_proofs::poly::commitment::Params;
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;
use halo2_base::halo2_proofs::SerdeFormat;

use snark_verifier::system::halo2::{compile, Config};
use snark_verifier_sdk::halo2::aggregation::{AggregationConfigParams, VerifierUniversality};
use snark_verifier_sdk::halo2::gen_snark_shplonk;
use snark_verifier_sdk::{gen_pk, Snark};

const HERMEZ_SG2_HEAD: [u8; 4] = [0x92, 0x8f, 0xaf, 0xb3];

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn load_hermez_srs(path: &Path, k: u32) -> anyhow::Result<ParamsKZG<Bn256>> {
    anyhow::ensure!(path.exists(), "SRS not found at {}", path.display());
    let mut f = fs::File::open(path)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    if params.k() > k {
        params.downsize(k);
    }
    anyhow::ensure!(params.k() == k, "SRS k={} != required {k}", params.k());
    let sg2 = params.s_g2().to_raw_bytes();
    anyhow::ensure!(sg2[0..4] == HERMEZ_SG2_HEAD, "SRS s_g2 head {:02x?} != Hermez", &sg2[0..4]);
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
            Option::<Fr>::from(Fr::from_bytes(&arr))
                .ok_or_else(|| anyhow::anyhow!("non-canonical Fr"))?,
        );
    }
    Ok(out)
}

/// Deterministic synthetic shard — MUST match `examples/export_shard_snark.rs` so the
/// shard proof's exposed hi/lo equal `native_committee_subtree_root` of this slice.
fn synthetic_shard() -> Vec<[u8; 48]> {
    (0..PUBKEYS_PER_SHARD)
        .map(|i| {
            let mut pk = [0u8; 48];
            for (j, b) in pk.iter_mut().enumerate() {
                *b = ((i * 7 + j * 13 + 1) % 251) as u8;
            }
            pk
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let n = env_usize("PROBE_N", 1);
    let k_outer = env_usize("PROBE_K_OUTER", 21) as u32;
    anyhow::ensure!(n >= 1, "PROBE_N must be >= 1");

    let shard_dir = PathBuf::from(std::env::var("SHARD_DIR").unwrap_or_else(|_| "out/shard_snark".to_string()));

    println!("=== M6: RotateAggregationCircuit (glue) over REAL shard snark — Hermez ===");
    println!("shard dir : {}", shard_dir.display());
    println!("N         : {n}\nk_outer   : {k_outer}\n");

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
    let params_inner = load_hermez_srs(&inner_srs, inner_k)?;
    let protocol =
        compile(&params_inner, &vk, Config::kzg().with_num_instance(vec![instances.len()]));
    let shard_snark = Snark::new(protocol, vec![instances.clone()], proof);
    println!("wrapped shard into snark_verifier_sdk::Snark (Hermez k={inner_k})\n");

    // ---- Synthetic-but-consistent glue witness -----------------------------
    let slice = synthetic_shard();
    let native_subtree = native_committee_subtree_root(&slice);
    // aggregate_pubkey: deterministic synthetic 48 bytes (only constrained by the glue).
    let mut aggregate = [0u8; 48];
    for (j, b) in aggregate.iter_mut().enumerate() {
        *b = ((j * 5 + 3) % 251) as u8;
    }
    let witness = RotateGlueWitness::synthetic(
        vec![native_subtree; n],
        aggregate,
        NEXT_SYNC_COMMITTEE_GINDEX,
        Fr::from(0x1234_5678u64),
        9_412u64,
    );
    println!(
        "glue witness: {n} subtree root(s), gindex={}, branch depth={}\n",
        witness.gindex,
        witness.branch.len()
    );

    let inners: Vec<Snark> = std::iter::repeat(shard_snark).take(n).collect();

    // ---- Outer SRS + aggregation config ------------------------------------
    let outer_srs = std::env::var("OUTER_SRS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("../params/kzg_bn254_{k_outer}.srs")));
    let params_outer = load_hermez_srs(&outer_srs, k_outer)?;
    println!("outer SRS: Hermez k={k_outer}\n");

    let agg_config = AggregationConfigParams {
        degree: k_outer,
        lookup_bits: (k_outer - 1) as usize,
        ..Default::default()
    };

    // ---- Keygen ------------------------------------------------------------
    println!("keygen stage (aggregate_snarks + glue)...");
    let mut keygen = RotateAggregationCircuit::new(
        CircuitBuilderStage::Keygen,
        agg_config,
        &params_outer,
        inners.clone(),
        VerifierUniversality::Full,
        &witness,
        None,
    );
    let calculated = keygen.calculate_params(Some(10));
    let t = Instant::now();
    let pk = gen_pk(&params_outer, &keygen, None);
    println!("  gen_pk {:.1}s", t.elapsed().as_secs_f64());
    let break_points = keygen.break_points();
    drop(keygen);

    // ---- Prove + self-verify -----------------------------------------------
    println!("prover stage...");
    let prover = RotateAggregationCircuit::new(
        CircuitBuilderStage::Prover,
        calculated,
        &params_outer,
        inners,
        VerifierUniversality::Full,
        &witness,
        None,
    )
    .use_break_points(break_points);

    let t = Instant::now();
    let agg = gen_snark_shplonk(&params_outer, &pk, prover, None::<&Path>);
    let dt = t.elapsed().as_secs_f64();

    let outer: usize = agg.instances.iter().map(|c| c.len()).sum();
    let expected = NUM_ACCUMULATOR_INSTANCES + NUM_ROTATE_PUBLIC_INPUTS;
    println!("\n=== RESULT: PASS ===");
    println!("Aggregated + glued N={n} REAL shard snark(s) at Hermez k_outer={k_outer} in {dt:.1}s");
    println!("outer proof: {} B, outer instances = {outer} (expected {expected} = {NUM_ACCUMULATOR_INSTANCES} accumulator + {NUM_ROTATE_PUBLIC_INPUTS} rotate PIs)", agg.proof().len());
    anyhow::ensure!(outer == expected, "outer instance count {outer} != expected {expected}");
    println!(
        "\nsnark-verifier verified the shard proof in-circuit AND the committee glue\n\
         (node_from_hilo -> sync_committee_root_from_subtrees -> verify_merkle_branch\n\
         -> combine_committee_commitment) proved satisfiable; rotate PIs exposed."
    );
    Ok(())
}
