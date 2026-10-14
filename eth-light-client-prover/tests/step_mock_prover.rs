//! M3 step-circuit MockProver test on real mainnet data.
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test step_mock_prover -- --ignored --nocapture
//! ```
//!
//! Uses the **real** attested/finalized `BeaconBlockHeader`s and the **real**
//! `finality_branch` from the fixture (mainnet Fulu and Sepolia Gloas) (so the SSZ signing-root + finality-branch
//! paths run on live consensus data), plus a **synthetic-but-valid** 512-member
//! committee that actually signs the real `signing_root`. This proves the whole
//! step circuit is satisfiable and sound end-to-end without needing to fetch the
//! exact mainnet sync committee (that comes in the M6 testnet E2E).

use eth_light_client_prover::fork::{
    EXECUTION_BRANCH_LEN, FINALITY_BRANCH_LEN, GLOAS_NOT_SCHEDULED, SEPOLIA_GLOAS_FORK_SLOT,
};
use eth_light_client_prover::live_witness::{synthetic_step_witness, update_headers, ChainParams};
use eth_light_client_prover::ssz::load_node;
use eth_light_client_prover::step::{
    pack_step_instances, verify_step, StepWitness, STEP_GLOAS_FORK_SLOT_INDEX, STEP_INSTANCE_LEN,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::gates::flex_gate::threads::SinglePhaseCoreManager;
use halo2_base::gates::{RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::dev::MockProver;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::plonk::{keygen_pk, keygen_vk};
use halo2_base::halo2_proofs::SerdeFormat;
use halo2_base::utils::fs::gen_srs;
use halo2_base::utils::testing::{check_proof_with_instances, gen_proof_with_instances};
use halo2_base::AssignedValue;
use std::time::Instant;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");
/// Live Sepolia `finality_update` after the Gloas fork (attested and finalized
/// headers both Gloas).
const SEPOLIA_GLOAS_FIXTURE: &str = include_str!("../fixtures/sepolia/finality_update_gloas.json");

fn witness_for(fixture: &str, params: &ChainParams) -> StepWitness {
    synthetic_step_witness(update_headers(fixture, params.gloas_fork_slot).unwrap(), params)
}

/// Mainnet Fulu: the Electra/Deneb indices.
fn build_witness() -> StepWitness {
    witness_for(FIXTURE, &ChainParams::MAINNET_FULU)
}

/// Sepolia Gloas: the Gloas indices.
fn build_gloas_witness() -> StepWitness {
    witness_for(SEPOLIA_GLOAS_FIXTURE, &ChainParams::SEPOLIA_GLOAS)
}

#[test]
fn native_step_witness_is_consistent() {
    for w in [build_witness(), build_gloas_witness()] {
        assert_eq!(w.pubkeys.len(), 512);
        assert_eq!(w.pubkeys_compressed.len(), 512);
        assert_eq!(w.finality_branch.len(), FINALITY_BRANCH_LEN);
        assert_eq!(w.execution_branch.len(), EXECUTION_BRANCH_LEN);
    }
    assert_eq!(build_witness().gloas_fork_slot, GLOAS_NOT_SCHEDULED);
    assert_eq!(build_gloas_witness().gloas_fork_slot, SEPOLIA_GLOAS_FORK_SLOT);
}

/// Reading the Sepolia Gloas update at the pre-Gloas indices must fail natively:
/// its 9-node finality branch has no zero padding to strip.
#[test]
fn gloas_update_does_not_parse_at_electra_indices() {
    assert!(update_headers(SEPOLIA_GLOAS_FIXTURE, GLOAS_NOT_SCHEDULED).is_err());
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
const SYNTH_GLOAS_FORK_SLOT: u64 = 11_296_768;

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
        Fr::from(SYNTH_GLOAS_FORK_SLOT),
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
    let g = ctx.load_witness(Fr::from(SYNTH_GLOAS_FORK_SLOT));
    pack_step_instances(ctx, gate, a, f, &fbr, p, c, &bh, &asr, g)
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

fn mock_prove_step(w: &StepWitness, instances_override: Option<Vec<Fr>>) -> Result<(), String> {
    let k = 23u32;
    let lb = 22usize;

    let mut builder =
        BaseCircuitBuilder::<Fr>::default().use_k(k as usize).use_lookup_bits(lb).use_instance_columns(1);
    let range = RangeChip::new(lb, builder.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);

    let pi = verify_step(builder.pool(0), &range, &sha, w);
    assert_eq!(pi.instances.len(), STEP_INSTANCE_LEN);
    let vals = instances_override.unwrap_or_else(|| pi.instances.iter().map(|c| *c.value()).collect());
    builder.assigned_instances[0] = pi.instances.clone();

    builder.config_params.lookup_bits = Some(lb);
    builder.calculate_params(Some(9));

    MockProver::run(k, &builder, vec![vals]).unwrap().verify().map_err(|e| format!("{e:?}"))
}

#[test]
#[ignore = "full step circuit (BLS + SSZ + subgroup + fusion) with instance column; run on n14, tune k"]
fn step_verifies_on_real_fixture() {
    mock_prove_step(&build_witness(), None).unwrap();
}

/// The live Sepolia Gloas update, at gindices 735 (finality) and 2856
/// (execution block hash).
#[test]
#[ignore = "full step circuit on the Sepolia Gloas fixture; run on n14"]
fn step_verifies_on_sepolia_gloas_fixture() {
    let w = build_gloas_witness();
    mock_prove_step(&w, None).unwrap();
}

/// A Gloas witness claiming Gloas has not started: the circuit reads the
/// branches at the Electra/Deneb indices and must reject them.
#[test]
#[ignore = "full step circuit, negative; run on n14"]
fn step_rejects_gloas_branches_under_a_wrong_fork_slot() {
    let mut w = build_gloas_witness();
    w.gloas_fork_slot = GLOAS_NOT_SCHEDULED;
    assert!(mock_prove_step(&w, None).is_err());
}

/// A pre-Gloas witness claiming Gloas started at genesis: rejected likewise.
#[test]
#[ignore = "full step circuit, negative; run on n14"]
fn step_rejects_fulu_branches_under_a_wrong_fork_slot() {
    let mut w = build_witness();
    w.gloas_fork_slot = 0;
    assert!(mock_prove_step(&w, None).is_err());
}

/// The fork slot is a public input: the proof does not verify against another.
#[test]
#[ignore = "full step circuit, negative; run on n14"]
fn step_binds_the_gloas_fork_slot_instance() {
    let w = build_gloas_witness();
    let mut vals = {
        let mut b = BaseCircuitBuilder::<Fr>::default().use_k(23).use_lookup_bits(22).use_instance_columns(1);
        let range = RangeChip::new(22, b.lookup_manager().clone());
        let sha = Sha256Chip::new(&range);
        verify_step(b.pool(0), &range, &sha, &w).instances.iter().map(|c| *c.value()).collect::<Vec<_>>()
    };
    assert_eq!(vals[STEP_GLOAS_FORK_SLOT_INDEX], Fr::from(SEPOLIA_GLOAS_FORK_SLOT));
    vals[STEP_GLOAS_FORK_SLOT_INDEX] += Fr::ONE;
    assert!(mock_prove_step(&w, Some(vals)).is_err());
}
