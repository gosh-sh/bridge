//! M6 recursive rotate — the aggregation circuit that snark-verifies N committee
//! **shard** proofs and finishes the committee glue in-circuit.
//!
//! ## What this closes
//!
//! Each shard proof ([`crate::rotate::verify_shard`]) exposes 3 public instances
//! `[subtree_root_hi, subtree_root_lo, shard_digest]` bound to its 64 pubkey
//! byte-witnesses. This circuit:
//!
//! 1. **snark-verifies** the N shard proofs via snark-verifier's
//!    [`aggregate_snarks`] (BN254 SHPLONK accumulation; the verified inner public
//!    instances come back as private assigned cells in `previous_instances`).
//! 2. **reconstructs** each shard's 32-byte subtree `Node` from its exposed
//!    `(hi, lo)` limbs — 32 range-checked byte witnesses, constrained so
//!    `LE(bytes[0..16]) == hi` and `LE(bytes[16..32]) == lo` (binds the bytes to
//!    the verified instances).
//! 3. runs the cheap SHA top [`sync_committee_root_from_subtrees`] over the N
//!    reconstructed subtree roots + `aggregate_pubkey` → the committee SSZ root.
//! 4. [`verify_merkle_branch`] binds that committee root to a beacon `state_root`
//!    at [`crate::rotate::NEXT_SYNC_COMMITTEE_GINDEX`] (= 87).
//! 5. [`combine_committee_commitment`] folds the N shard Poseidon digests +
//!    aggregate into the committee commitment (`next_commit`).
//! 6. exposes rotate public inputs `[current_commit, next_commit, period]` after
//!    the 12-limb KZG accumulator.
//!
//! ## Accumulator (KZG decider)
//!
//! Like every snark-verifier aggregation, the outer proof is sound only if a
//! verifier also runs the **pairing decider** over the 12 accumulator limbs
//! (`accumulator_indices()` marks them at instance offsets `0..12`). An EVM Yul
//! verifier bakes this in; whether the AN-side `ZKHALO2VERIFYWITHVK` opcode runs
//! the decider for an *aggregation* VkBlob is an open consumption question tracked
//! in `docs/m6_rotate_recursive.md`. A plain SHPLONK verify proves the circuit was
//! satisfied but not the inner-proof pairing, so it must not be the final gate.
//!
//! Feature-gated behind `aggregation` (pulls `snark-verifier{,-sdk}`).

use crate::committee::{native_sync_committee_root_from_subtrees, sync_committee_root_from_subtrees};
use crate::rotate::{combine_committee_commitment, new_committee_hasher, SHARD_INSTANCE_LEN};
use crate::ssz::{
    load_bytes, load_node, merkle_depth, native_merkle_branch_root, verify_merkle_branch, Node,
};
use crate::step::STEP_INSTANCE_LEN;

use gosh_sha256_chip::Sha256Chip;

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::{BaseCircuitParams, BaseConfig, CircuitBuilderStage};
use halo2_base::gates::flex_gate::MultiPhaseThreadBreakPoints;
use halo2_base::gates::{GateInstructions, RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::circuit::{Layouter, SimpleFloorPlanner};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr};
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::plonk::{self, Circuit, ConstraintSystem, Selector};
use halo2_base::halo2_proofs::poly::commitment::ParamsProver;
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;
use halo2_base::{AssignedValue, Context, QuantumCell};

use snark_verifier_sdk::halo2::aggregation::{
    aggregate_snarks, AggregationConfigParams, Svk, VerifierUniversality,
};
use snark_verifier_sdk::{CircuitExt, Snark, SHPLONK};

/// The 12-limb KZG accumulator snark-verifier exposes as the first outer instances
/// (4 EC coordinates × 3 limbs). A verifier must run the pairing decider over these.
pub const NUM_ACCUMULATOR_INSTANCES: usize = 12;

/// Rotate public inputs appended after the accumulator: `[current_commit,
/// next_commit, period]` (R2 self-contained layout — see `EthBeaconLightClient`).
pub const NUM_ROTATE_PUBLIC_INPUTS: usize = 3;

/// Index of the signing committee's Poseidon `committee_commitment` in a verified
/// step proof's public instances (`crate::step::pack_step_instances`). When a step
/// snark is folded, the rotate `current_commit` is bound to this cell.
pub const STEP_COMMITTEE_COMMITMENT_INDEX: usize = 5;

/// Indices of the attested beacon `state_root` (hi/lo) in a verified step proof's
/// public instances. When a step snark is folded, the `next_sync_committee_branch`
/// anchor is bound to this state_root — the state the current committee *signed* —
/// so the successor committee is proven to sit in the signed state (`bls-state-root`).
pub const STEP_ATTESTED_STATE_ROOT_HI_INDEX: usize = 8;
pub const STEP_ATTESTED_STATE_ROOT_LO_INDEX: usize = 9;

/// Off-circuit witnesses the aggregation needs beyond the shard snarks: the native
/// subtree roots (to reconstruct byte-`Node`s, bound to the verified hi/lo), the
/// aggregate pubkey, and the `next_sync_committee` branch + beacon `state_root`.
#[derive(Clone, Debug)]
pub struct RotateGlueWitness {
    /// Native 32-byte subtree root per shard (order matches the snark order).
    pub subtree_roots: Vec<[u8; 32]>,
    /// `aggregate_pubkey` (48 compressed bytes).
    pub aggregate: [u8; 48],
    /// `next_sync_committee` SSZ Merkle branch (`floor(log2(gindex))` nodes).
    pub branch: Vec<[u8; 32]>,
    /// Beacon `state_root` the branch reconstructs. Used as the anchor witness when
    /// no step snark is folded; when a step snark IS folded (`bls-state-root`), the
    /// anchor is bound to the step's verified attested `state_root` and this value is
    /// only range-checked as the byte pre-image (must equal the signed state_root).
    pub state_root: [u8; 32],
    /// Generalized index of the committee field (87 for `next_sync_committee`).
    pub gindex: u64,
    /// Previous committee commitment (R2 passthrough). Used only when the circuit
    /// is built WITHOUT a step snark; when a step snark is folded (`bls-bind`),
    /// `current_commit` is bound to the verified step proof's `committee_commitment`
    /// and this witness value is ignored.
    pub current_commit: Fr,
    /// Sync-committee period (R2 passthrough).
    pub period: u64,
}

impl RotateGlueWitness {
    /// Build a self-consistent synthetic witness for the mechanics bring-up: derive
    /// the committee root from the subtree roots + aggregate, fabricate a branch,
    /// and fold it to the matching `state_root` so [`verify_merkle_branch`] passes.
    pub fn synthetic(
        subtree_roots: Vec<[u8; 32]>,
        aggregate: [u8; 48],
        gindex: u64,
        current_commit: Fr,
        period: u64,
    ) -> Self {
        let committee_root = native_sync_committee_root_from_subtrees(&subtree_roots, &aggregate);
        let depth = merkle_depth(gindex);
        let branch: Vec<[u8; 32]> = (0..depth)
            .map(|i| {
                let mut n = [0u8; 32];
                n[0] = (i as u8).wrapping_add(1);
                n[31] = 0xAB;
                n
            })
            .collect();
        let state_root = native_merkle_branch_root(&committee_root, &branch, gindex);
        Self { subtree_roots, aggregate, branch, state_root, gindex, current_commit, period }
    }
}

/// Little-endian byte-cells → single field element (`Σ b_i · 256^i`); mirror of the
/// private `rotate::le_field` used to *produce* the shard's hi/lo, so the recomposed
/// value matches byte-for-byte.
fn le_field(
    ctx: &mut Context<Fr>,
    gate: &impl GateInstructions<Fr>,
    bytes: &[AssignedValue<Fr>],
) -> AssignedValue<Fr> {
    let mut acc = ctx.load_constant(Fr::ZERO);
    let mut base = Fr::ONE;
    for &b in bytes {
        let term = gate.mul(ctx, b, QuantumCell::Constant(base));
        acc = gate.add(ctx, acc, term);
        base *= Fr::from(256u64);
    }
    acc
}

/// Reconstruct a shard's 32-byte subtree `Node` from its verified `(hi, lo)` limbs:
/// witness the 32 native bytes, range-check each to 8 bits, and constrain
/// `LE(bytes[0..16]) == hi`, `LE(bytes[16..32]) == lo`. This binds the byte-`Node`
/// (fed to the SHA top) to the snark-verified public instances.
fn node_from_hilo(
    ctx: &mut Context<Fr>,
    range: &RangeChip<Fr>,
    hi: AssignedValue<Fr>,
    lo: AssignedValue<Fr>,
    native_root: &[u8; 32],
) -> Node<Fr> {
    let gate = range.gate();
    let bytes: Vec<AssignedValue<Fr>> =
        native_root.iter().map(|&b| ctx.load_witness(Fr::from(b as u64))).collect();
    for &byte in &bytes {
        range.range_check(ctx, byte, 8);
    }
    let hi2 = le_field(ctx, gate, &bytes[0..16]);
    let lo2 = le_field(ctx, gate, &bytes[16..32]);
    ctx.constrain_equal(&hi2, &hi);
    ctx.constrain_equal(&lo2, &lo);
    bytes
}

/// Aggregation circuit: verify N committee-shard snarks + finish the committee glue.
///
/// Newtype over [`BaseCircuitBuilder`] (mirrors snark-verifier-sdk's own
/// `AggregationCircuit`) so we can insert the committee glue between
/// [`aggregate_snarks`] and setting the public instances, and expose the correct
/// [`CircuitExt::accumulator_indices`].
pub struct RotateAggregationCircuit {
    pub builder: BaseCircuitBuilder<Fr>,
}

impl RotateAggregationCircuit {
    /// Build the circuit for `stage`. `snarks` are the N committee-shard/tree-node
    /// inputs (`witness.subtree_roots` must be in the same order). When `step_snark`
    /// is `Some`, it is folded as an ADDITIONAL inner input (a light-client step
    /// proof, [`STEP_INSTANCE_LEN`] instances) and TWO bindings replace witnessed
    /// values (R2 opt-1): the rotate `current_commit` PI is bound to the step's
    /// verified `committee_commitment` (`bls-bind`), and the
    /// `next_sync_committee_branch` anchor `state_root` is bound to the step's
    /// verified attested `state_root` (`bls-state-root`). When `None`, both fall back
    /// to `witness.current_commit` / `witness.state_root`.
    pub fn new(
        stage: CircuitBuilderStage,
        config_params: AggregationConfigParams,
        params: &ParamsKZG<Bn256>,
        snarks: Vec<Snark>,
        universality: VerifierUniversality,
        witness: &RotateGlueWitness,
        step_snark: Option<Snark>,
    ) -> Self {
        let svk: Svk = params.get_g()[0].into();
        let mut builder =
            BaseCircuitBuilder::<Fr>::from_stage(stage).use_params(config_params.into());
        let range = builder.range_chip();

        // 1. snark-verify the inner snarks (folds any inner accumulator by
        //    `snark.protocol.accumulator_indices`). The first `n_committee_inputs`
        //    are EITHER leaf shard snarks (SHARD_INSTANCE_LEN instances, direct) OR
        //    intermediate tree nodes (`[NUM_ACCUMULATOR_INSTANCES accumulator ++
        //    m·SHARD_INSTANCE_LEN passthrough]`); an optional trailing step snark
        //    (STEP_INSTANCE_LEN instances) binds `current_commit`.
        let n_committee_inputs = snarks.len();
        let has_step = step_snark.is_some();
        let mut all_snarks = snarks;
        if let Some(s) = step_snark {
            all_snarks.push(s);
        }
        let out = aggregate_snarks::<SHPLONK>(builder.pool(0), &range, svk, all_snarks, universality);
        let total_shards = witness.subtree_roots.len();

        let sha = Sha256Chip::new(&range);
        let rotate_pis: Vec<AssignedValue<Fr>> = {
            let gate = range.gate();
            let ctx = builder.main(0);
            let hasher = new_committee_hasher(ctx, gate);

            // 2. reconstruct subtree Nodes (bound to verified hi/lo) + collect digests.
            //    Each inner surfaces one-or-more shard triples; the tree root strips
            //    the leading 12 accumulator limbs before chunking into triples.
            let mut nodes: Vec<Node<Fr>> = Vec::with_capacity(total_shards);
            let mut digests: Vec<AssignedValue<Fr>> = Vec::with_capacity(total_shards);
            let mut idx = 0usize;
            for prev in out.previous_instances[..n_committee_inputs].iter() {
                let triples: &[AssignedValue<Fr>] = if prev.len() == SHARD_INSTANCE_LEN {
                    &prev[..]
                } else {
                    assert!(
                        prev.len() > NUM_ACCUMULATOR_INSTANCES
                            && (prev.len() - NUM_ACCUMULATOR_INSTANCES) % SHARD_INSTANCE_LEN == 0,
                        "tree-node prev instances {} not [12 acc ++ k·{SHARD_INSTANCE_LEN}]",
                        prev.len()
                    );
                    &prev[NUM_ACCUMULATOR_INSTANCES..]
                };
                for c in triples.chunks(SHARD_INSTANCE_LEN) {
                    nodes.push(node_from_hilo(ctx, &range, c[0], c[1], &witness.subtree_roots[idx]));
                    digests.push(c[2]);
                    idx += 1;
                }
            }
            assert_eq!(idx, total_shards, "collected {idx} shard triples != {total_shards}");

            // 3. cheap SHA top → committee SSZ root.
            let agg_cells = load_bytes(ctx, &witness.aggregate);
            let committee_root = sync_committee_root_from_subtrees(&sha, ctx, nodes, &agg_cells);

            // 4. bind committee root to the beacon state_root via the branch. When a
            //    step snark is folded, the anchor state_root is BOUND to the step's
            //    verified attested `state_root` (indices 8/9) — the state the current
            //    committee signed — so the successor committee is proven to sit in the
            //    signed state (bls-state-root); else it is witnessed (mechanics).
            let branch: Vec<Node<Fr>> = witness.branch.iter().map(|b| load_node(ctx, b)).collect();
            let state_root = if has_step {
                let sp = &out.previous_instances[n_committee_inputs];
                assert_eq!(
                    sp.len(),
                    STEP_INSTANCE_LEN,
                    "step snark must expose {STEP_INSTANCE_LEN} instances, got {}",
                    sp.len()
                );
                node_from_hilo(
                    ctx,
                    &range,
                    sp[STEP_ATTESTED_STATE_ROOT_HI_INDEX],
                    sp[STEP_ATTESTED_STATE_ROOT_LO_INDEX],
                    &witness.state_root,
                )
            } else {
                load_node(ctx, &witness.state_root)
            };
            verify_merkle_branch(&sha, ctx, &committee_root, &branch, witness.gindex, &state_root);

            // 5. + 6. commitments + rotate PIs. `current_commit` is BOUND to the
            //    folded step proof's verified `committee_commitment` when present
            //    (bls-bind, R2 opt-1), else witnessed (mechanics bring-up).
            let next_commit = combine_committee_commitment(&hasher, ctx, gate, &digests, &agg_cells);
            let current_commit = if has_step {
                let sp = &out.previous_instances[n_committee_inputs];
                assert_eq!(
                    sp.len(),
                    STEP_INSTANCE_LEN,
                    "step snark must expose {STEP_INSTANCE_LEN} instances, got {}",
                    sp.len()
                );
                sp[STEP_COMMITTEE_COMMITMENT_INDEX]
            } else {
                ctx.load_witness(witness.current_commit)
            };
            let period = ctx.load_witness(Fr::from(witness.period));
            vec![current_commit, next_commit, period]
        };

        assert_eq!(builder.assigned_instances.len(), 1, "one instance column");
        let mut instances = out.accumulator;
        instances.extend(rotate_pis);
        builder.assigned_instances[0] = instances;

        Self { builder }
    }

    /// Auto-configure and return the refined params (call after keygen-stage build).
    pub fn calculate_params(&mut self, minimum_rows: Option<usize>) -> AggregationConfigParams {
        self.builder.calculate_params(minimum_rows).try_into().unwrap()
    }

    /// Break points computed during keygen synthesize (read after `gen_pk`).
    pub fn break_points(&self) -> MultiPhaseThreadBreakPoints {
        self.builder.break_points()
    }

    /// Set break points (prover stage).
    pub fn set_break_points(&mut self, break_points: MultiPhaseThreadBreakPoints) {
        self.builder.set_break_points(break_points);
    }

    /// Builder-style break-point setter.
    pub fn use_break_points(mut self, break_points: MultiPhaseThreadBreakPoints) -> Self {
        self.set_break_points(break_points);
        self
    }
}

impl Circuit<Fr> for RotateAggregationCircuit {
    type Config = BaseConfig<Fr>;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = BaseCircuitParams;

    fn params(&self) -> BaseCircuitParams {
        self.builder.config_params.clone()
    }

    fn without_witnesses(&self) -> Self {
        unimplemented!()
    }

    fn configure_with_params(
        meta: &mut ConstraintSystem<Fr>,
        params: BaseCircuitParams,
    ) -> BaseConfig<Fr> {
        <BaseCircuitBuilder<Fr> as Circuit<Fr>>::configure_with_params(meta, params)
    }

    fn configure(_: &mut ConstraintSystem<Fr>) -> BaseConfig<Fr> {
        unreachable!()
    }

    fn synthesize(
        &self,
        config: BaseConfig<Fr>,
        layouter: impl Layouter<Fr>,
    ) -> Result<(), plonk::Error> {
        self.builder.synthesize(config, layouter)
    }
}

impl CircuitExt<Fr> for RotateAggregationCircuit {
    fn num_instance(&self) -> Vec<usize> {
        self.builder.num_instance()
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        self.builder.instances()
    }

    fn accumulator_indices() -> Option<Vec<(usize, usize)>> {
        Some((0..NUM_ACCUMULATOR_INSTANCES).map(|i| (0, i)).collect())
    }

    fn selectors(config: &BaseConfig<Fr>) -> Vec<Selector> {
        <BaseCircuitBuilder<Fr> as CircuitExt<Fr>>::selectors(config)
    }
}
