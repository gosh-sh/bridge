//! M6 R2 opt-1 (`bls-bind`) — emit a light-client **"step"** proof under the
//! **Poseidon** Fiat–Shamir transcript so it can be snark-verified as an inner
//! input to the recursive rotate aggregation (`RotateAggregationCircuit`).
//!
//! The step circuit ([`eth_light_client_prover::step::verify_step`]) proves the
//! sync-committee **signed** the attested header (BLS aggregate + 2/3 supermajority
//! + finality branch) and exposes, at public-input index 5, the Poseidon
//! `committee_commitment` of the **signing** committee. The rotate aggregation
//! folds this proof and binds its rotate PI `current_commit` to that verified
//! commitment instead of witnessing it (see `examples/rotate_tree_n8.rs`).
//!
//! Same transcript/ceremony discipline as `export_shard_snark.rs`: **Poseidon**
//! transcript (byte-identical to snark-verifier-sdk's) + **Hermez** SRS so the
//! outer decider's `[s]G2` matches. Step keygens at Hermez **k=21** (deliberately
//! high so the proof has few advice columns → cheap for the rotate root to fold).
//!
//! ## Emits (`out/step_snark/`)
//! - `step_vk.bin`        — `VerifyingKey::write(RawBytesUnchecked)`
//! - `step_config.json`   — the carried `BaseCircuitParams`
//! - `step_proof.bin`     — SHPLONK proof bytes under the Poseidon transcript
//! - `step_instances.bin` — `STEP_INSTANCE_LEN × 32` LE `Fr`
//!
//! ## Run (n14)
//! ```bash
//! STEP_SRS_PATH=$HOME/srs/hermez-raw-21 \
//!   cargo run --release --features aggregation --example export_step_snark
//! ```
//! Env: `STEP_SRS_PATH` (default `data/kzg_params_21.srs`), `STEP_OUT_DIR`
//! (default `out/step_snark`).

use std::fs;
use std::path::Path;
use std::time::Instant;

use eth_light_client_prover::bls_core::{signing_root_to_g2, verify_native};
use eth_light_client_prover::execution::ExecutionPayloadVals;
use eth_light_client_prover::mainnet::find_update_fixture;
use eth_light_client_prover::poseidon_transcript::{PoseidonChallenge, PoseidonRead, PoseidonWrite};
use eth_light_client_prover::signing::{
    native_sync_committee_signing_root, FORK_VERSION_FULU, MAINNET_GENESIS_VALIDATORS_ROOT,
};
use eth_light_client_prover::step::{verify_step, HeaderVals, StepWitness, STEP_INSTANCE_LEN};

use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::gates::RangeChip;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{
    G1Affine as BlsG1Affine, G2Affine as BlsG2Affine, G1 as BlsG1, G2 as BlsG2,
};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::halo2curves::group::{Curve, Group};
use halo2_base::halo2_proofs::halo2curves::serde::SerdeObject;
use halo2_base::halo2_proofs::plonk::{
    create_proof, keygen_pk, keygen_vk, verify_proof, VerifyingKey,
};
use halo2_base::halo2_proofs::poly::commitment::{Params, ParamsProver};
use halo2_base::halo2_proofs::poly::kzg::commitment::{KZGCommitmentScheme, ParamsKZG};
use halo2_base::halo2_proofs::poly::kzg::multiopen::{ProverSHPLONK, VerifierSHPLONK};
use halo2_base::halo2_proofs::poly::kzg::strategy::SingleStrategy;
use halo2_base::halo2_proofs::SerdeFormat;
use rand::rngs::OsRng;
use serde_json::Value;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");
/// Keygen the step at **k=21** (not the k=19 used for the standalone opcode
/// VkBlob): the in-circuit verifier cost the rotate root pays to fold this snark
/// is proportional to the proof's commitment count ≈ `num_advice`, which scales
/// as `1/2^k`. At k=19 the step has ~132 advice columns; at k=21 it drops to ~33
/// — comparable to one L2 node, so the 3-input root (2×L2 + step) fits k=21.
const K: u32 = 21;
const LOOKUP_BITS: usize = 18;
/// Hermez `[s]·G2` raw-bytes head — the ceremony snark-verifier's decider needs.
const HERMEZ_SG2_HEAD: [u8; 4] = [0x92, 0x8f, 0xaf, 0xb3];

fn h32(s: &str) -> [u8; 32] {
    hex::decode(s.trim_start_matches("0x")).unwrap().try_into().unwrap()
}
fn hexv(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap()
}
fn u64s(v: &Value) -> u64 {
    v.as_str().unwrap().parse().unwrap()
}
fn u256_le(s: &str) -> [u8; 32] {
    let val: u128 = s.parse().unwrap();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&val.to_le_bytes());
    out
}
fn header(data: &Value, which: &str) -> HeaderVals {
    let b = &data[which]["beacon"];
    HeaderVals {
        slot: b["slot"].as_str().unwrap().parse().unwrap(),
        proposer_index: b["proposer_index"].as_str().unwrap().parse().unwrap(),
        parent_root: h32(b["parent_root"].as_str().unwrap()),
        state_root: h32(b["state_root"].as_str().unwrap()),
        body_root: h32(b["body_root"].as_str().unwrap()),
    }
}
fn execution(data: &Value, which: &str) -> ExecutionPayloadVals {
    let e = &data[which]["execution"];
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
fn execution_branch(data: &Value, which: &str) -> Vec<[u8; 32]> {
    data[which]["execution_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect()
}

/// Build the step witness with a synthetic (self-consistent) signing committee.
///
/// The attested/finalized headers, finality branch and execution payload are the
/// **real** ones from the SAME update fixture the rotate driver consumes
/// (`mainnet::find_update_fixture()` → `update_period_*.json`, falling back to the
/// embedded `finality_update.json`). Driving both from one update makes the step's
/// exposed attested `state_root` == the rotate branch anchor, so `bls-state-root`
/// binds. The VK is witness-independent, so the synthetic committee still yields the
/// production VK; only the exposed `committee_commitment` sample depends on it.
fn build_witness() -> (StepWitness, [u8; 32], String) {
    let (raw, label) = match find_update_fixture() {
        Some(path) => (
            std::fs::read_to_string(&path).unwrap(),
            path.file_name().unwrap().to_string_lossy().into_owned(),
        ),
        None => (FIXTURE.to_string(), "finality_update.json (embedded)".to_string()),
    };
    let v: Value = serde_json::from_str(&raw).unwrap();
    // Fixtures are either `{"data": {...}}` or `[{"data": {...}}]`.
    let data = if v.is_array() { v[0]["data"].clone() } else { v["data"].clone() };

    let attested = header(&data, "attested_header");
    let finalized = header(&data, "finalized_header");
    let attested_state_root = attested.state_root;
    let finality_branch: Vec<[u8; 32]> = data["finality_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect();

    let signing_root = native_sync_committee_signing_root(
        attested.slot,
        attested.proposer_index,
        &attested.parent_root,
        &attested.state_root,
        &attested.body_root,
        &FORK_VERSION_FULU,
        &MAINNET_GENESIS_VALIDATORS_ROOT,
    );
    let msg_hash = signing_root_to_g2(&signing_root);
    let hm = BlsG2::from(msg_hash);

    let mut pubkeys: Vec<BlsG1Affine> = Vec::with_capacity(512);
    let mut agg = BlsG1::identity();
    let mut sig = BlsG2::identity();
    for _ in 0..512 {
        let sk = <BlsG1 as Group>::Scalar::random(OsRng);
        pubkeys.push((BlsG1::generator() * sk).to_affine());
        agg += BlsG1::generator() * sk;
        sig += hm * sk;
    }
    let signature: BlsG2Affine = sig.to_affine();
    assert!(verify_native(&signature, &agg.to_affine(), &msg_hash), "synthetic committee must verify");

    let pubkeys_compressed: Vec<[u8; 48]> = pubkeys.iter().map(|p| p.to_compressed_be()).collect();
    let aggregate_pubkey = agg.to_affine().to_compressed_be();

    let w = StepWitness {
        attested,
        finalized,
        finality_branch,
        fork_version: FORK_VERSION_FULU,
        genesis_validators_root: MAINNET_GENESIS_VALIDATORS_ROOT,
        pubkeys,
        pubkeys_compressed,
        aggregate_pubkey,
        bits: vec![true; 512],
        signature,
        finalized_execution: execution(&data, "finalized_header"),
        execution_branch: execution_branch(&data, "finalized_header"),
    };
    (w, attested_state_root, label)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Build one step circuit; `prover` carries (config, break_points) for the prover
/// stage, `None` for keygen.
fn build_step(
    w: &StepWitness,
    prover: Option<(halo2_base::gates::circuit::BaseCircuitParams, Vec<Vec<usize>>)>,
) -> (BaseCircuitBuilder<Fr>, Vec<Fr>) {
    let mut b = match &prover {
        Some((config, bp)) => {
            BaseCircuitBuilder::<Fr>::prover(config.clone(), bp.clone()).use_instance_columns(1)
        }
        None => BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
            .use_k(K as usize)
            .use_lookup_bits(LOOKUP_BITS)
            .use_instance_columns(1),
    };
    let range = RangeChip::new(LOOKUP_BITS, b.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(b.pool(0), &range, &sha, w);
    assert_eq!(pi.instances.len(), STEP_INSTANCE_LEN);
    let inst: Vec<Fr> = pi.instances.iter().map(|c| *c.value()).collect();
    b.assigned_instances[0] = pi.instances.clone();
    b.config_params.lookup_bits = Some(LOOKUP_BITS);
    (b, inst)
}

fn main() -> anyhow::Result<()> {
    println!("=== M6 bls-bind: export step Poseidon SHPLONK snark (Hermez k={K}) ===\n");

    let srs_path =
        std::env::var("STEP_SRS_PATH").unwrap_or_else(|_| "data/kzg_params_21.srs".to_string());
    let out_dir = std::env::var("STEP_OUT_DIR").unwrap_or_else(|_| "out/step_snark".to_string());
    fs::create_dir_all(&out_dir)?;

    // ---- Load Hermez SRS ----------------------------------------------------
    anyhow::ensure!(Path::new(&srs_path).exists(), "Hermez SRS not found at {srs_path}");
    println!("Loading SRS {srs_path} ...");
    let mut f = fs::File::open(&srs_path)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    if params.k() > K {
        println!("  downsizing {} -> {K}", params.k());
        params.downsize(K);
    }
    anyhow::ensure!(params.k() == K, "SRS k={} != required {K}", params.k());
    let sg2 = params.s_g2().to_raw_bytes();
    anyhow::ensure!(
        sg2[0..4] == HERMEZ_SG2_HEAD,
        "SRS s_g2 head {:02x?} != Hermez {:02x?}",
        &sg2[0..4],
        HERMEZ_SG2_HEAD
    );
    println!("  OK: Hermez ceremony, k={K}\n");

    let (w, attested_state_root, fixture_label) = build_witness();
    println!(
        "step update fixture: {fixture_label}\n  attested state_root = 0x{} (rotate branch anchor)\n",
        hex::encode(attested_state_root)
    );

    // ---- Keygen -------------------------------------------------------------
    let (mut kb, _) = build_step(&w, None);
    let config = kb.calculate_params(Some(9));
    println!("Step circuit config: {config:?}\n");

    let t = Instant::now();
    let vk = keygen_vk(&params, &kb).map_err(|e| anyhow::anyhow!("keygen_vk: {e:?}"))?;
    println!("keygen_vk {:?}", t.elapsed());
    let t = Instant::now();
    let pk = keygen_pk(&params, vk, &kb).map_err(|e| anyhow::anyhow!("keygen_pk: {e:?}"))?;
    println!("keygen_pk {:?}", t.elapsed());
    let break_points = kb.break_points();
    drop(kb);

    // ---- Prove (Poseidon transcript) ---------------------------------------
    let (pb, inst) = build_step(&w, Some((config.clone(), break_points)));
    assert_eq!(inst.len(), STEP_INSTANCE_LEN);
    println!("step instances: {STEP_INSTANCE_LEN} × Fr (committee_commitment = inst[5])");

    let instances: &[&[Fr]] = &[&inst];
    let t = Instant::now();
    let mut transcript = PoseidonWrite::init(Vec::<u8>::new());
    create_proof::<KZGCommitmentScheme<Bn256>, ProverSHPLONK<'_, Bn256>, PoseidonChallenge, _, _, _>(
        &params,
        &pk,
        &[pb],
        &[instances],
        OsRng,
        &mut transcript,
    )
    .map_err(|e| anyhow::anyhow!("create_proof (Poseidon): {e:?}"))?;
    let proof = transcript.finalize();
    println!("prove (Poseidon transcript) {:?} -> {} B", t.elapsed(), proof.len());

    // ---- Self-verify (Poseidon transcript) ---------------------------------
    println!("\nSelf-check (VerifierSHPLONK + Poseidon transcript)...");
    let strategy = SingleStrategy::new(&params);
    let mut vt = PoseidonRead::init(proof.as_slice());
    let ok = verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        PoseidonChallenge,
        PoseidonRead<&[u8]>,
        SingleStrategy<'_, Bn256>,
    >(params.verifier_params(), pk.get_vk(), strategy, &[instances], &mut vt)
    .is_ok();
    anyhow::ensure!(ok, "Poseidon SHPLONK verify_proof REJECTED the step proof");
    println!("  OK: step Poseidon proof verifies\n");

    // ---- Write artifacts ----------------------------------------------------
    let mut vk_bytes = Vec::new();
    pk.get_vk()
        .write(&mut vk_bytes, SerdeFormat::RawBytesUnchecked)
        .map_err(|e| anyhow::anyhow!("vk write: {e}"))?;
    let mut slice_rd: &[u8] = &vk_bytes;
    let _vk_rt = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut slice_rd,
        SerdeFormat::RawBytesUnchecked,
        config.clone(),
    )
    .map_err(|e| anyhow::anyhow!("VK does not read back via BaseCircuitBuilder<Fr>: {e}"))?;

    let mut pi_bytes = Vec::with_capacity(inst.len() * 32);
    for fr in &inst {
        pi_bytes.extend_from_slice(fr.to_bytes().as_ref());
    }

    fs::write(format!("{out_dir}/step_vk.bin"), &vk_bytes)?;
    fs::write(format!("{out_dir}/step_config.json"), serde_json::to_vec_pretty(&config)?)?;
    fs::write(format!("{out_dir}/step_proof.bin"), &proof)?;
    fs::write(format!("{out_dir}/step_instances.bin"), &pi_bytes)?;

    println!("=== RESULT: PASS — step Poseidon snark artifacts written to {out_dir} ===");
    println!("  step_vk.bin ({} B, sha256 {})", vk_bytes.len(), sha256_hex(&vk_bytes));
    println!("  step_proof.bin ({} B, sha256 {})", proof.len(), sha256_hex(&proof));
    let hexbe = |f: &Fr| hex::encode(f.to_bytes().iter().rev().copied().collect::<Vec<u8>>());
    println!(
        "  step_instances.bin ({} B = {} × 32 Fr)\n    committee_commitment (inst[5])  = 0x{}\n    attested_state_root  (inst[8|9]) = hi 0x{} lo 0x{}",
        pi_bytes.len(),
        inst.len(),
        hexbe(&inst[5]),
        hexbe(&inst[8]),
        hexbe(&inst[9]),
    );
    Ok(())
}
