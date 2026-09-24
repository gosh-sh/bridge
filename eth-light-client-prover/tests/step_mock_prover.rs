//! M3 step-circuit MockProver test on real mainnet data.
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test step_mock_prover -- --ignored --nocapture
//! ```
//!
//! Uses the **real** attested/finalized `BeaconBlockHeader`s and the **real**
//! `finality_branch` from the fixture (so the SSZ signing-root + finality-branch
//! paths run on live consensus data), plus a **synthetic-but-valid** 512-member
//! committee that actually signs the real `signing_root`. This proves the whole
//! step circuit is satisfiable and sound end-to-end without needing to fetch the
//! exact mainnet sync committee (that comes in the M6 testnet E2E).

use eth_light_client_prover::bls_core::{signing_root_to_g2, verify_native};
use eth_light_client_prover::execution::ExecutionPayloadVals;
use eth_light_client_prover::signing::{
    native_sync_committee_signing_root, FORK_VERSION_FULU, MAINNET_GENESIS_VALIDATORS_ROOT,
};
use eth_light_client_prover::ssz::load_node;
use eth_light_client_prover::step::{
    pack_step_instances, verify_step, HeaderVals, StepWitness, STEP_INSTANCE_LEN,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::gates::flex_gate::threads::SinglePhaseCoreManager;
use halo2_base::gates::{RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::dev::MockProver;
use halo2_base::halo2_proofs::halo2curves::bls12_381::{G1Affine, G2Affine, G1, G2};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::halo2curves::group::{Curve, Group};
use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk};
use halo2_base::halo2_proofs::SerdeFormat;
use halo2_base::utils::fs::gen_srs;
use halo2_base::utils::testing::{check_proof_with_instances, gen_proof_with_instances};
use halo2_base::AssignedValue;
use rand::rngs::OsRng;
use serde_json::Value;
use std::time::Instant;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");

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

fn header(v: &Value, which: &str) -> HeaderVals {
    let b = &v["data"][which]["beacon"];
    HeaderVals {
        slot: b["slot"].as_str().unwrap().parse().unwrap(),
        proposer_index: b["proposer_index"].as_str().unwrap().parse().unwrap(),
        parent_root: h32(b["parent_root"].as_str().unwrap()),
        state_root: h32(b["state_root"].as_str().unwrap()),
        body_root: h32(b["body_root"].as_str().unwrap()),
    }
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

fn build_witness() -> StepWitness {
    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    let attested = header(&v, "attested_header");
    let finalized = header(&v, "finalized_header");
    let finality_branch: Vec<[u8; 32]> = v["data"]["finality_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect();

    // The exact message the committee must sign.
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
    let hm = G2::from(msg_hash);

    // Synthetic valid committee: 512 signers, all participating.
    let mut pubkeys: Vec<G1Affine> = Vec::with_capacity(512);
    let mut agg = G1::identity();
    let mut sig = G2::identity();
    for _ in 0..512 {
        let sk = <G1 as Group>::Scalar::random(OsRng);
        pubkeys.push((G1::generator() * sk).to_affine());
        agg += G1::generator() * sk;
        sig += hm * sk;
    }
    let signature: G2Affine = sig.to_affine();
    assert!(verify_native(&signature, &agg.to_affine(), &msg_hash), "synthetic committee must verify");

    // Compressed encodings for the decode-bind + Poseidon commitment.
    let pubkeys_compressed: Vec<[u8; 48]> = pubkeys.iter().map(|p| p.to_compressed_be()).collect();
    let aggregate_pubkey = agg.to_affine().to_compressed_be();

    StepWitness {
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
        finalized_execution: execution(&v, "finalized_header"),
        execution_branch: execution_branch(&v, "finalized_header"),
    }
}

#[test]
fn native_step_witness_is_consistent() {
    let w = build_witness();
    assert_eq!(w.pubkeys.len(), 512);
    assert_eq!(w.pubkeys_compressed.len(), 512);
    assert_eq!(w.finality_branch.len(), 7);
    assert_eq!(w.execution_branch.len(), 4);
}

/// Build a circuit with one instance column, run `build` to produce the instance
/// cells, wire them, and MockProver-check against `instance_vals` (defaults to the
/// cells' own values). `expect_ok=false` asserts the instance check *fails*.
fn run_with_instances(
    k: u32,
    lb: usize,
    build: impl FnOnce(&mut SinglePhaseCoreManager<Fr>, &RangeChip<Fr>) -> Vec<AssignedValue<Fr>>,
    instance_vals: Option<Vec<Fr>>,
    expect_ok: bool,
) {
    let mut builder =
        BaseCircuitBuilder::<Fr>::default().use_k(k as usize).use_lookup_bits(lb).use_instance_columns(1);
    let range = RangeChip::new(lb, builder.lookup_manager().clone());

    let cells = build(builder.pool(0), &range);
    let vals = instance_vals.unwrap_or_else(|| cells.iter().map(|c| *c.value()).collect());
    builder.assigned_instances[0] = cells;

    // Mirror base_test: disable the lookup table if unused (cheap layout test).
    let used = builder.lookup_manager().iter().map(|lm| lm.total_rows()).sum::<usize>();
    builder.config_params.lookup_bits = if used == 0 { None } else { Some(lb) };
    builder.calculate_params(Some(9));

    let prover = MockProver::run(k, &builder, vec![vals]).unwrap();
    if expect_ok {
        prover.assert_satisfied();
    } else {
        assert!(prover.verify().is_err(), "tampered instance must be rejected");
    }
}

// Synthetic public-input pieces for the fast layout/binding test.
const SYNTH_ATTESTED_SLOT: u64 = 7_000_001;
const SYNTH_FINALIZED_SLOT: u64 = 7_000_000;
const SYNTH_PARTICIPATION: u64 = 400;
const SYNTH_COMMIT: u64 = 0xdead_beef_dead_beef;

fn synth_fbr() -> [u8; 32] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(1))
}
fn synth_bh() -> [u8; 32] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(3))
}
fn synth_asr() -> [u8; 32] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(13).wrapping_add(5))
}

fn le16(bytes: &[u8]) -> Fr {
    let mut acc = Fr::ZERO;
    let mut base = Fr::ONE;
    for &b in bytes {
        acc += Fr::from(b as u64) * base;
        base *= Fr::from(256u64);
    }
    acc
}

/// Native twin of [`pack_step_instances`] — pins the layout order + hi/lo endianness.
fn native_instances() -> Vec<Fr> {
    let fbr = synth_fbr();
    let bh = synth_bh();
    let asr = synth_asr();
    vec![
        Fr::from(SYNTH_ATTESTED_SLOT),
        Fr::from(SYNTH_FINALIZED_SLOT),
        le16(&fbr[0..16]),
        le16(&fbr[16..32]),
        Fr::from(SYNTH_PARTICIPATION),
        Fr::from(SYNTH_COMMIT),
        le16(&bh[0..16]),
        le16(&bh[16..32]),
        le16(&asr[0..16]),
        le16(&asr[16..32]),
    ]
}

fn build_synth_instances(
    pool: &mut SinglePhaseCoreManager<Fr>,
    range: &RangeChip<Fr>,
) -> Vec<AssignedValue<Fr>> {
    let ctx = pool.main();
    let gate = range.gate();
    let a = ctx.load_witness(Fr::from(SYNTH_ATTESTED_SLOT));
    let f = ctx.load_witness(Fr::from(SYNTH_FINALIZED_SLOT));
    let p = ctx.load_witness(Fr::from(SYNTH_PARTICIPATION));
    let c = ctx.load_witness(Fr::from(SYNTH_COMMIT));
    let fbr = load_node(ctx, &synth_fbr());
    let bh = load_node(ctx, &synth_bh());
    let asr = load_node(ctx, &synth_asr());
    pack_step_instances(ctx, gate, a, f, &fbr, p, c, &bh, &asr)
}

/// Fast test: the instance layout is exactly [`STEP_INSTANCE_LEN`] elements in the
/// canonical order (matching the native twin), the circuit-produced cells are
/// exposed on the instance column, and tampering any position is rejected.
#[test]
fn step_instances_layout_and_binding() {
    let expected = native_instances();
    assert_eq!(expected.len(), STEP_INSTANCE_LEN);

    // Positive: explicit native instance values (pins order + endianness).
    run_with_instances(12, 11, build_synth_instances, Some(expected.clone()), true);

    // Negative: every position must be genuinely bound to the instance column.
    for i in 0..STEP_INSTANCE_LEN {
        let mut tampered = expected.clone();
        tampered[i] += Fr::ONE;
        run_with_instances(12, 11, build_synth_instances, Some(tampered), false);
    }
}

/// Shape probe: build the fused witness at a **chain-ceremony-compatible**
/// `lookup_bits` (≤18, so the range table fits `2^19`) and report the real advice
/// / lookup cell counts plus the auto-calculated column count per candidate `k`.
/// This answers whether the fused step can be configured at `k ≤ 19` (the chain
/// Powers-of-Tau ceiling) by widening columns, or whether a constant-size wrapper
/// is required for the AN-side `ZKHALO2VERIFYWITHVK` verifier.
#[test]
#[ignore = "shape probe: fused witness gen + per-k column report; run on n14"]
fn step_circuit_shape() {
    let w = build_witness();
    let lb = 18usize; // range table 2^18 < 2^19 (chain ceremony ceiling)

    let mut builder =
        BaseCircuitBuilder::<Fr>::default().use_k(24).use_lookup_bits(lb).use_instance_columns(1);
    let range = RangeChip::new(lb, builder.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(builder.pool(0), &range, &sha, &w);
    builder.assigned_instances[0] = pi.instances.clone();

    let stats = builder.statistics();
    let total_advice: usize = stats.gate.total_advice_per_phase.iter().sum();
    let total_lookup: usize = stats.total_lookup_advice_per_phase.iter().sum();
    println!("SHAPE lookup_bits={lb}");
    println!(
        "  total_advice_per_phase={:?} (sum {total_advice})",
        stats.gate.total_advice_per_phase
    );
    println!("  total_fixed={}", stats.gate.total_fixed);
    println!(
        "  total_lookup_advice_per_phase={:?} (sum {total_lookup})",
        stats.total_lookup_advice_per_phase
    );
    for k in [19usize, 20, 21, 22, 23] {
        let p = builder.core().calculate_params(k, Some(9));
        let max_rows = (1usize << k) - 9;
        let num_lookup: Vec<usize> = stats
            .total_lookup_advice_per_phase
            .iter()
            .map(|c| c.div_ceil(max_rows))
            .collect();
        println!(
            "  k={k}: num_advice_per_phase={:?} num_fixed={} num_lookup_advice={:?}",
            p.num_advice_per_phase, p.num_fixed, num_lookup
        );
    }
}

/// Real SHPLONK prover round-trip at **k=19** (the chain Powers-of-Tau ceiling):
/// keygen → prove (with the 8 instances) → verify. Confirms the fused step is
/// real-prover-sound and, crucially, **fits the chain ceremony size** (so the
/// AN-side `ZKHALO2VERIFYWITHVK` opcode can consume it directly, no wrapper).
/// Uses a fresh test SRS (`gen_srs`) — provability + sizes are tau-independent;
/// the chain-tau specifics matter only for actual opcode consumption (M5/M6).
#[test]
#[ignore = "real k=19 SHPLONK keygen+prove+verify; heavy, run on n14"]
fn step_real_proof_k19() {
    let w = build_witness();
    let k = 19u32;
    let lb = 18usize;

    // Keygen circuit.
    let mut kb = BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
        .use_k(k as usize)
        .use_lookup_bits(lb)
        .use_instance_columns(1);
    let range = RangeChip::new(lb, kb.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(kb.pool(0), &range, &sha, &w);
    kb.assigned_instances[0] = pi.instances.clone();
    kb.config_params.lookup_bits = Some(lb);
    let config = kb.calculate_params(Some(9));
    println!("REAL k={k} config: {config:?}");

    let params = gen_srs(k);
    let t = Instant::now();
    let vk = keygen_vk(&params, &kb).expect("keygen_vk");
    println!("REAL keygen_vk {:?}", t.elapsed());
    let vk_bytes = vk.to_bytes(SerdeFormat::RawBytes).len();
    let t = Instant::now();
    let pk = keygen_pk(&params, vk, &kb).expect("keygen_pk");
    println!("REAL keygen_pk {:?}", t.elapsed());
    let break_points = kb.break_points();
    drop(kb);

    // Prover circuit (identical shape).
    let mut pb = BaseCircuitBuilder::prover(config, break_points).use_instance_columns(1);
    let range = RangeChip::new(lb, pb.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(pb.pool(0), &range, &sha, &w);
    let inst: Vec<Fr> = pi.instances.iter().map(|c| *c.value()).collect();
    pb.assigned_instances[0] = pi.instances.clone();

    let t = Instant::now();
    let proof = gen_proof_with_instances(&params, &pk, pb, &[&inst]);
    println!("REAL prove {:?}", t.elapsed());

    let t = Instant::now();
    check_proof_with_instances(&params, pk.get_vk(), &proof, &[&inst], true);
    println!("REAL verify {:?}", t.elapsed());

    println!(
        "REAL sizes: vk={vk_bytes} B, proof={} B, instances={}",
        proof.len(),
        inst.len()
    );
}

#[test]
#[ignore = "full step circuit (BLS + SSZ + subgroup + fusion) with instance column; run on n14, tune k"]
fn step_verifies_on_real_fixture() {
    let w = build_witness();
    let k = 23u32;
    let lb = 22usize;

    let mut builder =
        BaseCircuitBuilder::<Fr>::default().use_k(k as usize).use_lookup_bits(lb).use_instance_columns(1);
    let range = RangeChip::new(lb, builder.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);

    let pi = verify_step(builder.pool(0), &range, &sha, &w);
    assert_eq!(pi.instances.len(), STEP_INSTANCE_LEN);
    let vals: Vec<Fr> = pi.instances.iter().map(|c| *c.value()).collect();
    builder.assigned_instances[0] = pi.instances.clone();

    builder.config_params.lookup_bits = Some(lb);
    builder.calculate_params(Some(9));

    MockProver::run(k, &builder, vec![vals]).unwrap().assert_satisfied();
}
