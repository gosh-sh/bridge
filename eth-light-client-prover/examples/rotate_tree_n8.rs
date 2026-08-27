//! M6 recursive rotate — **full N=8 committee via the 2-to-1 recursion TREE.**
//!
//! Assembles the whole tree end to end, every node at `k=21` (peak RSS ≈ 40 GB —
//! see `docs/m6_rotate_recursive.md`), never a single `k=23` circuit:
//!
//! ```text
//!   8 shard snarks (leaves, k20)
//!        │  L1: stock AggregationCircuit, expose_previous_instances(false)
//!   4 ×  ▼  (aggregate 2 shards → 18 inst = 12 acc + 6 passthrough)
//!        │  L2: stock AggregationCircuit, expose_previous_instances(true)
//!   2 ×  ▼  (fold 2 L1 accumulators → 24 inst = 12 acc + 12 passthrough)
//!        │  ROOT: RotateAggregationCircuit — fold 2 L2 + committee glue
//!   1 ×  ▼  (8 subtree roots surface as prev[12..]; SHA top → merkle branch
//!           gindex 87 → Poseidon commit; emit [acc12, current, next, period])
//! ```
//!
//! The 8 leaves are **distinct** (`shard_{0..7}_*` from `export_shard_snark`):
//! the real mainnet `next_sync_committee` split into 8 balanced 64-pubkey slices
//! (falls back to 8 distinct synthetic slices when no fixture is present). The
//! root glues the **8 distinct subtree roots** and binds the recomposed committee
//! SSZ root to the beacon `state_root` via the **real `next_sync_committee_branch`**
//! @ gindex 87 — the full, faithful committee path.
//!
//! ## Run (n14, capture peak RSS across all levels)
//!
//! ```bash
//! cd eth-light-client-prover
//! /usr/bin/time -v env SHARD_DIR=out/shard_snark \
//!   OUTER_SRS=../params/kzg_bn254_21.srs TREE_K=21 \
//!   cargo run --release --features aggregation --example rotate_tree_n8 2>&1 | tail -60
//! ```
//!
//! Env: `SHARD_DIR` (default `out/shard_snark`), `OUTER_SRS`
//! (default `../params/kzg_bn254_{k}.srs`), `TREE_K` (default 21).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eth_light_client_prover::committee::{native_committee_subtree_root, COMMITTEE_SHARDS};
use eth_light_client_prover::mainnet::committee_source;
use eth_light_client_prover::rotate::{NEXT_SYNC_COMMITTEE_GINDEX, SHARD_INSTANCE_LEN};
use eth_light_client_prover::step::STEP_INSTANCE_LEN;
use eth_light_client_prover::rotate_aggregation::{
    RotateAggregationCircuit, RotateGlueWitness, NUM_ACCUMULATOR_INSTANCES, NUM_ROTATE_PUBLIC_INPUTS,
};

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::{BaseCircuitParams, CircuitBuilderStage};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::halo2curves::serde::SerdeObject;
use halo2_base::halo2_proofs::plonk::{verify_proof, VerifyingKey};
use halo2_base::halo2_proofs::poly::commitment::{Params, ParamsProver};
use halo2_base::halo2_proofs::poly::kzg::commitment::{KZGCommitmentScheme, ParamsKZG};
use halo2_base::halo2_proofs::poly::kzg::multiopen::VerifierSHPLONK;
use halo2_base::halo2_proofs::poly::kzg::strategy::SingleStrategy;
use halo2_base::halo2_proofs::transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer};
use halo2_base::halo2_proofs::SerdeFormat;
use halo2_base::utils::testing::gen_proof_with_instances;

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

/// Frozen `VkBlob` magic (see `deposit-prover/src/halo2_tvm_bundle.rs`).
const VK_BLOB_MAGIC: &[u8; 8] = b"VKBLOB\x00\x00";

fn write_chunk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Number of leading instances that form the snark-verifier KZG accumulator
/// (`lhs.x ‖ lhs.y ‖ rhs.x ‖ rhs.y`, 4 coords × 3 limbs). Flagged in the VkBlob
/// header so the opcode runs the pairing decider over `instances[0..12]`.
const VKBLOB_ACCUMULATOR_LIMBS: u8 = 12;

/// Base v1 VkBlob writer (byte-identical to `export_step_vk_blob.rs`): magic,
/// version=1 (Base), transcript=0 (Blake2b), shape=0, accumulator_limbs (byte
/// 11), reserved, config JSON, `VerifyingKey::write(RawBytes)`. The rotate root
/// is a `BaseCircuitBuilder` (newtype), so the Base v1 wire applies directly;
/// `accumulator_limbs = 12` marks it as a recursive-aggregation proof so the
/// opcode pairs the accumulator (soundness decider).
fn encode_base_v1_vkblob(config: &BaseCircuitParams, vk: &VerifyingKey<G1Affine>) -> Vec<u8> {
    let mut vk_bytes = Vec::new();
    vk.write(&mut vk_bytes, SerdeFormat::RawBytes).expect("VerifyingKey::write(RawBytes)");
    let cfg_json = serde_json::to_vec(config).expect("serialise BaseCircuitParams");
    let mut out = Vec::new();
    out.extend_from_slice(VK_BLOB_MAGIC);
    out.push(1); // version = 1 (Base)
    out.push(0); // transcript_kind = 0 (Blake2b)
    out.push(0); // circuit_shape = 0 (Base)
    out.push(VKBLOB_ACCUMULATOR_LIMBS); // byte 11: accumulator_limbs = 12 (KZG)
    out.extend_from_slice(&[0u8; 4]); // reserved
    write_chunk(&mut out, &cfg_json);
    write_chunk(&mut out, &vk_bytes);
    out
}

/// Reparse a Base v1 VkBlob → (BaseCircuitParams, vk_bytes); mirrors `VkBlob::read`.
fn parse_base_v1_vkblob(bytes: &[u8]) -> (BaseCircuitParams, Vec<u8>) {
    assert!(bytes.len() >= 16, "VkBlob too short");
    assert_eq!(&bytes[0..8], VK_BLOB_MAGIC, "VkBlob magic mismatch");
    assert_eq!(bytes[8], 1, "expected v1 (Base)");
    assert_eq!(bytes[9], 0, "expected Blake2b transcript");
    assert_eq!(bytes[10], 0, "v1 shape byte must be reserved 0");
    assert_eq!(
        bytes[11],
        VKBLOB_ACCUMULATOR_LIMBS,
        "rotate VkBlob byte 11 must be accumulator_limbs=12 (KZG decider flag)",
    );
    let mut off = 16;
    let cfg_len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let cfg: BaseCircuitParams = serde_json::from_slice(&bytes[off..off + cfg_len]).unwrap();
    off += cfg_len;
    let vk_len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let vk_bytes = bytes[off..off + vk_len].to_vec();
    (cfg, vk_bytes)
}


/// One stock intermediate aggregation node (L1 / L2). Returns a `Snark` whose
/// protocol carries `accumulator_indices = 0..12`, so a parent folds it.
fn agg_node(
    name: &str,
    params: &ParamsKZG<Bn256>,
    k: u32,
    inners: Vec<Snark>,
    has_prev_accumulator: bool,
) -> Snark {
    println!("── {name}: aggregate {} snark(s), has_prev={has_prev_accumulator}", inners.len());
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
    let snark = gen_snark_shplonk(params, &pk, pv, None::<&Path>);
    let ninst: usize = snark.instances.iter().map(|c| c.len()).sum();
    println!("   {name} done {:.1}s → {} B, {ninst} inst\n", t.elapsed().as_secs_f64(), snark.proof().len());
    snark
}

fn main() -> anyhow::Result<()> {
    let k = env_usize("TREE_K", 21) as u32;
    let shard_dir =
        PathBuf::from(std::env::var("SHARD_DIR").unwrap_or_else(|_| "out/shard_snark".to_string()));

    println!("=== M6: full N=8 committee via 2-to-1 recursion TREE ===");
    println!("shard dir : {}\ntree k    : {k}\n", shard_dir.display());

    // ---- Shared shard VK/config/protocol (all 8 shards share the shape) -----
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

    let inner_srs = shard_dir.join(format!("kzg_bn254_{inner_k}.srs"));
    let params_inner = load_srs(&inner_srs, inner_k)?;
    let protocol = compile(
        &params_inner,
        &vk,
        Config::kzg().with_num_instance(vec![SHARD_INSTANCE_LEN]),
    );

    // ---- 8 DISTINCT leaf shard snarks (shard_{i}_*) -------------------------
    let leaves: Vec<Snark> = (0..COMMITTEE_SHARDS)
        .map(|i| -> anyhow::Result<Snark> {
            let proof = fs::read(shard_dir.join(format!("shard_{i}_proof.bin"))).map_err(|e| {
                anyhow::anyhow!("shard_{i}_proof.bin: {e} (re-run export_shard_snark)")
            })?;
            let instances = read_instances(&shard_dir.join(format!("shard_{i}_instances.bin")))?;
            Ok(Snark::new(protocol.clone(), vec![instances], proof))
        })
        .collect::<anyhow::Result<_>>()?;
    println!(
        "loaded {} distinct leaf shards: k={inner_k}, {} B each",
        leaves.len(),
        leaves[0].proof().len()
    );

    // ---- Committee glue witness (real mainnet fixture, else synthetic) ------
    let source = committee_source();
    anyhow::ensure!(source.shards.len() == COMMITTEE_SHARDS, "committee source != 8 shards");
    let subtree_roots: Vec<[u8; 32]> =
        source.shards.iter().map(|s| native_committee_subtree_root(s)).collect();
    let witness = match &source.update {
        Some(fx) => {
            anyhow::ensure!(fx.branch.len() == 6, "next_sync_committee_branch depth != 6");
            RotateGlueWitness {
                subtree_roots: subtree_roots.clone(),
                aggregate: source.aggregate,
                branch: fx.branch.clone(),
                state_root: fx.state_root,
                gindex: NEXT_SYNC_COMMITTEE_GINDEX,
                // R2 passthrough: bound by a BLS/finality proof later (id: bls-bind).
                current_commit: Fr::from(0x1234_5678u64),
                period: fx.period(),
            }
        }
        None => RotateGlueWitness::synthetic(
            subtree_roots.clone(),
            source.aggregate,
            NEXT_SYNC_COMMITTEE_GINDEX,
            Fr::from(0x1234_5678u64),
            9_412u64,
        ),
    };
    println!(
        "committee: {} — {} distinct subtree roots, gindex={}, branch depth={}, period={}\n",
        source.label,
        witness.subtree_roots.len(),
        witness.gindex,
        witness.branch.len(),
        witness.period
    );

    let outer_srs = std::env::var("OUTER_SRS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("../params/kzg_bn254_{k}.srs")));
    let params = load_srs(&outer_srs, k)?;
    println!();

    // ---- Optional step snark → BIND current_commit (bls-bind, R2 opt-1) -----
    // When present, its verified `committee_commitment` (step PI[5]) replaces the
    // witnessed placeholder as the rotate `current_commit`.
    let step_dir =
        PathBuf::from(std::env::var("STEP_DIR").unwrap_or_else(|_| "out/step_snark".to_string()));
    let step_snark: Option<Snark> = if step_dir.join("step_proof.bin").exists() {
        let step_cfg: BaseCircuitParams =
            serde_json::from_slice(&fs::read(step_dir.join("step_config.json"))?)?;
        let step_k = step_cfg.k as u32;
        let step_vk_bytes = fs::read(step_dir.join("step_vk.bin"))?;
        let mut s: &[u8] = &step_vk_bytes;
        let step_vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut s,
            SerdeFormat::RawBytesUnchecked,
            step_cfg.clone(),
        )
        .map_err(|e| anyhow::anyhow!("read step VK: {e}"))?;
        let step_srs_path = std::env::var("STEP_SRS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("../params/kzg_bn254_{step_k}.srs")));
        let params_step = load_srs(&step_srs_path, step_k)?;
        let step_proof = fs::read(step_dir.join("step_proof.bin"))?;
        let step_inst = read_instances(&step_dir.join("step_instances.bin"))?;
        anyhow::ensure!(
            step_inst.len() == STEP_INSTANCE_LEN,
            "step instances {} != {STEP_INSTANCE_LEN}",
            step_inst.len()
        );
        let cc = step_inst[5];
        let step_protocol = compile(
            &params_step,
            &step_vk,
            Config::kzg().with_num_instance(vec![STEP_INSTANCE_LEN]),
        );
        println!(
            "step snark: k={step_k}, {} B → BIND current_commit = committee_commitment(inst[5]) = 0x{}\n",
            step_proof.len(),
            hex::encode(cc.to_bytes().iter().rev().copied().collect::<Vec<u8>>())
        );
        Some(Snark::new(step_protocol, vec![step_inst], step_proof))
    } else {
        println!(
            "no step snark at {} — current_commit stays WITNESSED (run export_step_snark to bind it)\n",
            step_dir.display()
        );
        None
    };
    let bound_current = step_snark.is_some();

    // ---- L1 (×4): aggregate distinct shard pairs {0,1}{2,3}{4,5}{6,7} -------
    let mut l1: Vec<Snark> = Vec::with_capacity(4);
    for p in 0..4 {
        let node = agg_node(
            &format!("L1[{p}]"),
            &params,
            k,
            vec![leaves[2 * p].clone(), leaves[2 * p + 1].clone()],
            false,
        );
        anyhow::ensure!(
            node.instances.iter().map(|c| c.len()).sum::<usize>() == 18,
            "L1[{p}] != 18 inst"
        );
        l1.push(node);
    }

    // ---- L2 (×2): fold L1 pairs {0,1}{2,3} (recursive accumulator fold) -----
    let mut l2: Vec<Snark> = Vec::with_capacity(2);
    for p in 0..2 {
        let node =
            agg_node(&format!("L2[{p}]"), &params, k, vec![l1[2 * p].clone(), l1[2 * p + 1].clone()], true);
        anyhow::ensure!(
            node.instances.iter().map(|c| c.len()).sum::<usize>() == 24,
            "L2[{p}] != 24 inst"
        );
        l2.push(node);
    }

    // ---- ROOT: fold 2×L2 (8 distinct shards surface) + committee glue -------
    let emit_vkblob = std::env::var("EMIT_VKBLOB").map(|v| v == "1" || v == "true").unwrap_or(false);
    println!("── ROOT: RotateAggregationCircuit over 2×L2 (8 distinct shards) + committee glue");
    let root_inners = vec![l2[0].clone(), l2[1].clone()];
    let cfg = AggregationConfigParams { degree: k, lookup_bits: (k - 1) as usize, ..Default::default() };
    let mut kg = RotateAggregationCircuit::new(
        CircuitBuilderStage::Keygen,
        cfg,
        &params,
        root_inners.clone(),
        VerifierUniversality::None,
        &witness,
        step_snark.clone(),
    );
    let calc = kg.calculate_params(Some(10));
    println!("   calculated: {calc:?}");
    let t = Instant::now();
    let pk = gen_pk(&params, &kg, None);
    println!("   gen_pk {:.1}s", t.elapsed().as_secs_f64());
    let bp = kg.break_points();
    drop(kg);

    // Keep copies for the optional Blake2b (opcode) re-prove.
    let bp_emit = if emit_vkblob { Some(bp.clone()) } else { None };
    let root_inners_emit = if emit_vkblob { Some(root_inners.clone()) } else { None };
    let step_snark_emit = if emit_vkblob { step_snark.clone() } else { None };

    let prover = RotateAggregationCircuit::new(
        CircuitBuilderStage::Prover,
        calc,
        &params,
        root_inners,
        VerifierUniversality::None,
        &witness,
        step_snark,
    )
    .use_break_points(bp);
    let t = Instant::now();
    let root = gen_snark_shplonk(&params, &pk, prover, None::<&Path>);
    let root_inst: usize = root.instances.iter().map(|c| c.len()).sum();
    let expected = NUM_ACCUMULATOR_INSTANCES + NUM_ROTATE_PUBLIC_INPUTS;
    println!("   ROOT prove {:.1}s → {} B, {root_inst} inst\n", t.elapsed().as_secs_f64(), root.proof().len());
    anyhow::ensure!(root_inst == expected, "root inst {root_inst} != {expected}");

    // ---- Persist the root snark for the decider reference + future emit -----
    let tree_out =
        PathBuf::from(std::env::var("TREE_OUT").unwrap_or_else(|_| "out/rotate_tree".to_string()));
    fs::create_dir_all(&tree_out)?;
    fs::write(tree_out.join("root.snark"), bincode::serialize(&root)?)?;
    let mut inst_bytes = Vec::with_capacity(root_inst * 32);
    for col in &root.instances {
        for v in col {
            inst_bytes.extend_from_slice(&v.to_bytes());
        }
    }
    fs::write(tree_out.join("root_instances.bin"), &inst_bytes)?;
    println!("saved root.snark ({} B) + root_instances.bin to {}\n", root.proof().len(), tree_out.display());

    // rotate PIs sit after the 12 accumulator limbs: [current, next, period].
    let pis = &root.instances[0];
    let hexbe = |f: &Fr| hex::encode(f.to_bytes().iter().rev().copied().collect::<Vec<u8>>());
    let (current, next, period) = (
        pis[NUM_ACCUMULATOR_INSTANCES],
        pis[NUM_ACCUMULATOR_INSTANCES + 1],
        pis[NUM_ACCUMULATOR_INSTANCES + 2],
    );
    println!("rotate PIs: current_commit = 0x{}", hexbe(&current));
    println!("            next_commit    = 0x{}", hexbe(&next));
    println!("            period         = 0x{}", hexbe(&period));

    // ---- EMIT (opcode): Blake2b outer proof + ROTATE_VK_BLOB ----------------
    if emit_vkblob {
        println!("\n── EMIT: re-prove ROOT under Blake2b + build ROTATE_VK_BLOB (opcode wire)");
        let base_params: BaseCircuitParams = calc.into();
        let prover2 = RotateAggregationCircuit::new(
            CircuitBuilderStage::Prover,
            calc,
            &params,
            root_inners_emit.expect("emit inners"),
            VerifierUniversality::None,
            &witness,
            step_snark_emit,
        )
        .use_break_points(bp_emit.expect("emit bp"));
        let inst: Vec<Fr> = root.instances[0].clone();
        let t = Instant::now();
        let proof = gen_proof_with_instances(&params, &pk, prover2, &[&inst]);
        println!("   Blake2b prove {:.1}s → {} B", t.elapsed().as_secs_f64(), proof.len());

        let vk_blob = encode_base_v1_vkblob(&base_params, pk.get_vk());
        anyhow::ensure!(
            vk_blob.len() >= 12 && vk_blob[11] == VKBLOB_ACCUMULATOR_LIMBS,
            "EMIT_VKBLOB wrote accumulator_limbs={} (byte 11), want {VKBLOB_ACCUMULATOR_LIMBS}; \
             a 0 here would ship an unsound rotate blob (opcode skips the KZG decider)",
            vk_blob.get(11).copied().unwrap_or(0xFF),
        );

        // Self-check the EXACT opcode Base read + SHPLONK verify path.
        let (cfg_back, vk_bytes_back) = parse_base_v1_vkblob(&vk_blob);
        anyhow::ensure!(cfg_back.k == base_params.k, "vkblob config.k round-trip");
        let mut slice: &[u8] = &vk_bytes_back;
        let vk_reread = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut slice,
            SerdeFormat::RawBytes,
            cfg_back.clone(),
        )
        .map_err(|e| anyhow::anyhow!("opcode-path VerifyingKey::read failed: {e}"))?;
        let strategy = SingleStrategy::new(&params);
        let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(proof.as_slice());
        let instance_refs: &[&[Fr]] = &[&inst];
        let ok = verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
            SingleStrategy<'_, Bn256>,
        >(params.verifier_params(), &vk_reread, strategy, &[instance_refs], &mut transcript)
        .is_ok();
        anyhow::ensure!(ok, "opcode-path Blake2b SHPLONK verify_proof REJECTED the rotate root proof");
        println!("   OK: ROTATE_VK_BLOB reads back (BaseCircuitBuilder<Fr>) + Blake2b SHPLONK verify ACCEPTS");

        fs::write(tree_out.join("rotate_vk_blob.bin"), &vk_blob)?;
        fs::write(tree_out.join("rotate_public_inputs.bin"), &inst_bytes)?;
        fs::write(tree_out.join("rotate_proof_blake2b.bin"), &proof)?;
        fs::write(tree_out.join("rotate_base_circuit_params.json"), serde_json::to_vec_pretty(&base_params)?)?;
        println!(
            "   wrote rotate_vk_blob.bin ({} B), rotate_public_inputs.bin ({} B = {} × 32 Fr), rotate_proof_blake2b.bin ({} B)",
            vk_blob.len(),
            inst_bytes.len(),
            inst.len(),
            proof.len()
        );
        println!(
            "   ⚠ opcode acceptance here is NECESSARY but NOT sufficient: the plain SHPLONK verify does\n\
             \x20    NOT pair instances[0..12] (the KZG accumulator). Soundness requires the partner opcode\n\
             \x20    extension (decider) — reference in examples/rotate_decider_check.rs (id: opcode-ext)."
        );
    }

    println!("\n=== RESULT: PASS ===");
    println!(
        "Full N=8 committee proved via 2-to-1 tree at k={k} (8 shards → 4 L1 → 2 L2 → 1 root).\n\
         Root folded 2 accumulator-carrying L2 snarks{}, surfaced all 8 subtree roots,\n\
         ran committee glue (SHA top → merkle branch gindex 87 → Poseidon commit), and\n\
         emitted rotate PIs [current, next, period] after the 12 accumulator limbs\n\
         (root = {root_inst} inst). Peak RSS from `/usr/bin/time -v` — NOT the k=23 monolith.",
        if bound_current { " + 1 step snark (current_commit BOUND to its committee_commitment)" } else { "" }
    );
    Ok(())
}
