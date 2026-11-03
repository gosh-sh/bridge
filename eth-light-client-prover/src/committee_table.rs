//! The rotate's committee circuit on the table SHA-256 of `zkevm-hashes`.
//!
//! One circuit at `k = 18` proves what the 8 committee shards and the first two
//! levels of the rotate tree proved before: for a committee of 512 compressed
//! pubkeys and its aggregate pubkey,
//!
//! 1. `hash_tree_root(SyncCommittee)`, the SSZ root of the committee;
//! 2. that root at `next_sync_committee` of a beacon `state_root` (gindex 87
//!    before Gloas, 2946 from it, picked by the attested slot against the Gloas
//!    fork slot);
//! 3. the committee's 2-level Poseidon commitment over the same pubkey bytes,
//!    the value `step` recomputes
//!    ([`crate::rotate::commit_sync_committee_2level`]).
//!
//! Public instances, [`COMMITTEE_INSTANCE_LEN`] BN254 scalars:
//!
//! ```text
//! [0] next_commit        [1] state_root_hi = LE(state_root[0..16])
//! [2] state_root_lo = LE(state_root[16..32])
//! [3] attested_slot      [4] gloas_fork_slot
//! ```
//!
//! `state_root_hi/lo` is the encoding of a step proof's `attested_state_root`
//! (step instances 8 and 9), so a consumer that verified a step proof of the
//! same update compares the two directly: the circuit does not know who signed
//! the state, the step proof does.
//!
//! Every hashed message is 64 bytes, which SHA-256 pads to two blocks. For
//! message `m`, blocks `2m` and `2m + 1` of the table circuit are tied to it
//! here: `is_final` is 0 then 1, the length is 64, the 16 input words of the
//! first block are the message bytes (each word four bytes, little-endian, as
//! the table circuit lays them out) and the digest of the second block is the
//! 32 byte cells its `hi`/`lo` halves decompose into (big-endian halves). Byte
//! cells are range-checked to 8 bits; the table circuit constrains the words
//! and the digest halves itself.

use std::cell::RefCell;

use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        flex_gate::MultiPhaseThreadBreakPoints,
        GateInstructions, RangeChip, RangeInstructions,
    },
    halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        halo2curves::{bn256::Fr, ff::Field},
        plonk::{Circuit, ConstraintSystem, Error},
    },
    AssignedValue, Context, QuantumCell,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zkevm_hashes::sha256::vanilla::{
    columns::Sha256CircuitConfig, param::SHA256_NUM_ROWS, witness::AssignedSha256Block,
};

use crate::{
    committee::{COMMITTEE_SHARDS, PUBKEYS_PER_SHARD, SYNC_COMMITTEE_SIZE},
    fork::{
        assert_is_gloas, is_gloas, NEXT_SYNC_COMMITTEE_GINDEX_ELECTRA,
        NEXT_SYNC_COMMITTEE_GINDEX_GLOAS, SYNC_COMMITTEE_BRANCH_LEN,
    },
    mainnet::RotateFixture,
    rotate::{combine_committee_commitment, commit_committee_shard, new_committee_hasher},
    ssz::merkle_depth,
};

/// Rows of the committee circuit.
pub const COMMITTEE_TABLE_K: usize = 18;
/// Public instances of the committee circuit.
pub const COMMITTEE_INSTANCE_LEN: usize = 5;
/// Range-check table of the base part: byte checks dominate.
const LOOKUP_BITS: usize = 8;
/// Rows halo2 keeps for blinding at this circuit's degree.
const NUM_UNUSABLE_ROWS: usize = 109;

/// Messages hashed, in circuit order: 512 pubkey leaves, the 511 inner nodes of
/// their tree, the aggregate pubkey leaf, the container node, then one per
/// branch level.
pub const NUM_MESSAGES: usize =
    SYNC_COMMITTEE_SIZE + (SYNC_COMMITTEE_SIZE - 1) + 1 + 1 + SYNC_COMMITTEE_BRANCH_LEN;
/// SHA-256 blocks: every message is 64 bytes, two blocks with the padding.
pub const NUM_BLOCKS: usize = 2 * NUM_MESSAGES;

/// What the circuit proves about one hand-off: the committee, the branch that
/// places it in `state_root`, and the slots that pick the branch's index.
#[derive(Clone, Debug)]
pub struct CommitteeWitness {
    pub pubkeys: Vec<[u8; 48]>,
    pub aggregate: [u8; 48],
    /// [`SYNC_COMMITTEE_BRANCH_LEN`] nodes in the layout
    /// [`RotateFixture::glue_branch`] returns.
    pub branch: Vec<[u8; 32]>,
    pub state_root: [u8; 32],
    pub attested_slot: u64,
    pub gloas_fork_slot: u64,
}

impl CommitteeWitness {
    pub fn from_update(update: &RotateFixture, gloas_fork_slot: u64) -> anyhow::Result<Self> {
        let w = Self {
            pubkeys: update.pubkeys.clone(),
            aggregate: update.aggregate,
            branch: update.glue_branch(gloas_fork_slot)?,
            state_root: update.state_root,
            attested_slot: update.attested_slot,
            gloas_fork_slot,
        };
        w.check()?;
        Ok(w)
    }

    /// A consistent witness over a pseudo-random committee and branch: the
    /// `state_root` is whatever they reconstruct.
    pub fn synthetic(seed: u64, attested_slot: u64, gloas_fork_slot: u64) -> Self {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut byte = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        };
        let mut w = Self {
            pubkeys: (0..SYNC_COMMITTEE_SIZE)
                .map(|_| std::array::from_fn(|_| byte()))
                .collect(),
            aggregate: std::array::from_fn(|_| byte()),
            branch: (0..SYNC_COMMITTEE_BRANCH_LEN)
                .map(|_| std::array::from_fn(|_| byte()))
                .collect(),
            state_root: [0; 32],
            attested_slot,
            gloas_fork_slot,
        };
        w.state_root = w.messages().1;
        w
    }

    fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pubkeys.len() == SYNC_COMMITTEE_SIZE,
            "{} pubkeys, a committee has {SYNC_COMMITTEE_SIZE}",
            self.pubkeys.len()
        );
        anyhow::ensure!(
            self.branch.len() == SYNC_COMMITTEE_BRANCH_LEN,
            "branch of {} nodes, the circuit takes {SYNC_COMMITTEE_BRANCH_LEN}",
            self.branch.len()
        );
        let (_, root) = self.messages();
        anyhow::ensure!(
            root == self.state_root,
            "the committee and branch reconstruct 0x{}, not state_root 0x{}",
            hex::encode(root),
            hex::encode(self.state_root)
        );
        Ok(())
    }

    fn use_gloas(&self) -> bool {
        is_gloas(self.attested_slot, self.gloas_fork_slot)
    }

    /// The 64-byte messages in circuit order and the root the branch ends in.
    pub fn messages(&self) -> (Vec<[u8; 64]>, [u8; 32]) {
        let mut msgs = Vec::with_capacity(NUM_MESSAGES);
        let mut level: Vec<[u8; 32]> = self
            .pubkeys
            .iter()
            .map(|pk| {
                let m = leaf_message(pk);
                msgs.push(m);
                sha(&m)
            })
            .collect();
        while level.len() > 1 {
            level = level
                .chunks(2)
                .map(|pair| {
                    let m = pair_message(&pair[0], &pair[1]);
                    msgs.push(m);
                    sha(&m)
                })
                .collect();
        }
        let agg = leaf_message(&self.aggregate);
        msgs.push(agg);
        let container = pair_message(&level[0], &sha(&agg));
        msgs.push(container);
        let mut node = sha(&container);
        let gloas = self.use_gloas();
        for (i, sibling) in self.branch.iter().enumerate() {
            let (sibling_left, active) = branch_level(i, gloas);
            let m = if sibling_left {
                pair_message(sibling, &node)
            } else {
                pair_message(&node, sibling)
            };
            msgs.push(m);
            if active {
                node = sha(&m);
            }
        }
        debug_assert_eq!(msgs.len(), NUM_MESSAGES);
        (msgs, node)
    }
}

fn sha(m: &[u8]) -> [u8; 32] {
    Sha256::digest(m).into()
}

/// `htr(Bytes48)`: the 48 bytes, then 16 zero bytes.
fn leaf_message(pk: &[u8; 48]) -> [u8; 64] {
    let mut m = [0u8; 64];
    m[..48].copy_from_slice(pk);
    m
}

fn pair_message(left: &[u8; 32], right: &[u8; 32]) -> [u8; 64] {
    let mut m = [0u8; 64];
    m[..32].copy_from_slice(left);
    m[32..].copy_from_slice(right);
    m
}

/// Level `i` of the `next_sync_committee` branch: whether the sibling is the
/// left input, and whether the level exists at the index in use. The branch
/// is as long as the deeper index; the shallower one leaves the node as it is
/// above its depth.
fn branch_level(i: usize, gloas: bool) -> (bool, bool) {
    let gindex = if gloas {
        NEXT_SYNC_COMMITTEE_GINDEX_GLOAS
    } else {
        NEXT_SYNC_COMMITTEE_GINDEX_ELECTRA
    };
    let active = i < merkle_depth(gindex);
    (active && (gindex >> i) & 1 == 1, active)
}

/// Parameters the circuit is configured with: fixed by the committee shape,
/// except the base part's columns, which keygen measures.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommitteeCircuitParams {
    pub base: BaseCircuitParams,
}

impl CommitteeCircuitParams {
    /// The base part's starting point before keygen sizes its columns.
    fn at_k() -> Self {
        Self {
            base: BaseCircuitParams {
                k: COMMITTEE_TABLE_K,
                lookup_bits: Some(LOOKUP_BITS),
                num_instance_columns: 1,
                ..Default::default()
            },
        }
    }
}

#[derive(Clone)]
pub struct CommitteeCircuitConfig {
    base: BaseConfig<Fr>,
    sha256: Sha256CircuitConfig<Fr>,
}

/// One block of the table circuit as cells of the base part.
struct LoadedBlock {
    is_final: AssignedValue<Fr>,
    length: AssignedValue<Fr>,
    words: Vec<AssignedValue<Fr>>,
    hash_hi: AssignedValue<Fr>,
    hash_lo: AssignedValue<Fr>,
}

pub struct CommitteeCircuit {
    witness: CommitteeWitness,
    params: CommitteeCircuitParams,
    builder: RefCell<BaseCircuitBuilder<Fr>>,
    instances: Vec<Fr>,
    sha_inputs: Option<Vec<[u8; 64]>>,
}

impl CommitteeCircuit {
    /// The circuit before keygen: `calculate_params` then sizes it.
    pub fn keygen(witness: CommitteeWitness) -> Self {
        Self::with(witness, CommitteeCircuitParams::at_k(), false, None)
    }

    /// The circuit for a proof with the parameters and break points of its key.
    pub fn prover(
        witness: CommitteeWitness,
        params: CommitteeCircuitParams,
        break_points: MultiPhaseThreadBreakPoints,
    ) -> Self {
        Self::with(witness, params, true, Some(break_points))
    }

    /// A circuit that only checks the constraints, for `MockProver`.
    pub fn mock(witness: CommitteeWitness) -> Self {
        let mut c = Self::keygen(witness);
        c.calculate_params();
        c
    }

    fn with(
        witness: CommitteeWitness,
        params: CommitteeCircuitParams,
        witness_gen_only: bool,
        break_points: Option<MultiPhaseThreadBreakPoints>,
    ) -> Self {
        let mut builder = BaseCircuitBuilder::new(witness_gen_only);
        builder.set_params(params.base.clone());
        if let Some(bp) = break_points {
            builder.set_break_points(bp);
        }
        let instances = native_instances(&witness);
        Self {
            witness,
            params,
            builder: RefCell::new(builder),
            instances,
            sha_inputs: None,
        }
    }

    /// Sizes the base part's columns by laying it out once over placeholder
    /// SHA-256 cells.
    pub fn calculate_params(&mut self) -> CommitteeCircuitParams {
        let blocks = {
            let b = self.builder.borrow();
            let mut copy = b.core().copy_manager.lock().unwrap();
            let mut cell = || copy.mock_external_assigned(Fr::ZERO);
            (0..NUM_BLOCKS)
                .map(|_| LoadedBlock {
                    is_final: cell(),
                    length: cell(),
                    words: (0..16).map(|_| cell()).collect(),
                    hash_hi: cell(),
                    hash_lo: cell(),
                })
                .collect::<Vec<_>>()
        };
        self.constrain(&blocks);
        let base = self
            .builder
            .borrow_mut()
            .calculate_params(Some(NUM_UNUSABLE_ROWS));
        self.builder.borrow_mut().clear();
        self.params = CommitteeCircuitParams {
            base,
        };
        self.params.clone()
    }

    pub fn params(&self) -> &CommitteeCircuitParams {
        &self.params
    }

    pub fn break_points(&self) -> MultiPhaseThreadBreakPoints {
        self.builder.borrow().break_points()
    }

    pub fn instances(&self) -> &[Fr] {
        &self.instances
    }

    /// Feeds the table circuit other messages than the witness hashes, so a
    /// test can check that the base part refuses them.
    #[doc(hidden)]
    pub fn with_sha_inputs(mut self, f: impl FnOnce(&mut Vec<[u8; 64]>)) -> Self {
        let mut msgs = self.witness.messages().0;
        f(&mut msgs);
        self.sha_inputs = Some(msgs);
        self
    }

    fn load_blocks(&self, assigned: Vec<AssignedSha256Block<'_, Fr>>) -> Vec<LoadedBlock> {
        let b = self.builder.borrow();
        let mut copy = b.core().copy_manager.lock().unwrap();
        assigned
            .into_iter()
            .map(|block| LoadedBlock {
                is_final: copy.load_external_assigned(block.is_final().clone()),
                length: copy.load_external_assigned(block.length().clone()),
                words: block
                    .word_values()
                    .iter()
                    .map(|w| copy.load_external_assigned(w.clone()))
                    .collect(),
                hash_hi: copy.load_external_assigned(block.output().hi()),
                hash_lo: copy.load_external_assigned(block.output().lo()),
            })
            .collect()
    }

    /// The base part: every constraint that is not SHA-256 itself.
    fn constrain(&self, blocks: &[LoadedBlock]) {
        assert_eq!(blocks.len(), NUM_BLOCKS);
        let w = &self.witness;
        let (msgs, _) = w.messages();
        let mut b = self.builder.borrow_mut();
        let range = RangeChip::new(LOOKUP_BITS, b.lookup_manager().clone());
        let gate = range.gate();
        let ctx = b.main(0);
        let mut hashed = Hashed {
            blocks,
            msgs: &msgs,
            next: 0,
        };

        let zero = ctx.load_zero();
        let pubkeys: Vec<Vec<AssignedValue<Fr>>> = w
            .pubkeys
            .iter()
            .map(|pk| load_checked_bytes(ctx, &range, pk))
            .collect();
        let aggregate = load_checked_bytes(ctx, &range, &w.aggregate);

        let mut level: Vec<Vec<AssignedValue<Fr>>> = pubkeys
            .iter()
            .map(|pk| {
                let m: Vec<_> = pk
                    .iter()
                    .copied()
                    .chain(std::iter::repeat(zero).take(16))
                    .collect();
                hashed.hash(ctx, &range, &m)
            })
            .collect();
        while level.len() > 1 {
            level = level
                .chunks(2)
                .map(|pair| {
                    let m: Vec<_> = pair[0].iter().chain(&pair[1]).copied().collect();
                    hashed.hash(ctx, &range, &m)
                })
                .collect();
        }
        let agg_msg: Vec<_> = aggregate
            .iter()
            .copied()
            .chain(std::iter::repeat(zero).take(16))
            .collect();
        let agg_root = hashed.hash(ctx, &range, &agg_msg);
        let container: Vec<_> = level[0].iter().chain(&agg_root).copied().collect();
        let mut node = hashed.hash(ctx, &range, &container);

        let attested_slot = ctx.load_witness(Fr::from(w.attested_slot));
        let gloas_fork_slot = ctx.load_witness(Fr::from(w.gloas_fork_slot));
        let use_gloas = assert_is_gloas(ctx, &range, attested_slot, gloas_fork_slot);
        let use_electra = gate.not(ctx, use_gloas);
        for (i, sibling) in w.branch.iter().enumerate() {
            let sibling = load_checked_bytes(ctx, &range, sibling);
            let (left_g, active_g) = branch_level(i, true);
            let (left_e, active_e) = branch_level(i, false);
            let sibling_left = select_const(ctx, use_gloas, use_electra, left_g, left_e);
            let active = select_const(ctx, use_gloas, use_electra, active_g, active_e);
            let mut m = Vec::with_capacity(64);
            for j in 0..32 {
                m.push(gate.select(ctx, sibling[j], node[j], sibling_left));
            }
            for j in 0..32 {
                m.push(gate.select(ctx, node[j], sibling[j], sibling_left));
            }
            let digest = hashed.hash(ctx, &range, &m);
            node = (0..32)
                .map(|j| gate.select(ctx, digest[j], node[j], active))
                .collect();
        }
        assert_eq!(hashed.next, NUM_MESSAGES);

        let state_root = load_checked_bytes(ctx, &range, &w.state_root);
        for (a, b) in node.iter().zip(&state_root) {
            ctx.constrain_equal(a, b);
        }
        let state_root_hi = le_int(ctx, gate, &state_root[0..16]);
        let state_root_lo = le_int(ctx, gate, &state_root[16..32]);

        let hasher = new_committee_hasher(ctx, gate);
        let flat: Vec<_> = pubkeys.iter().flatten().copied().collect();
        let digests: Vec<_> = flat
            .chunks(PUBKEYS_PER_SHARD * 48)
            .map(|slice| commit_committee_shard(&hasher, ctx, gate, slice))
            .collect();
        debug_assert_eq!(digests.len(), COMMITTEE_SHARDS);
        let next_commit = combine_committee_commitment(&hasher, ctx, gate, &digests, &aggregate);

        let out = vec![
            next_commit,
            state_root_hi,
            state_root_lo,
            attested_slot,
            gloas_fork_slot,
        ];
        debug_assert_eq!(
            out.iter().map(|c| *c.value()).collect::<Vec<_>>(),
            self.instances,
            "circuit instances differ from the native ones"
        );
        b.assigned_instances[0] = out;
    }
}

/// Hands out the table circuit's blocks two per message, in order.
struct Hashed<'a> {
    blocks: &'a [LoadedBlock],
    msgs: &'a [[u8; 64]],
    next: usize,
}

impl Hashed<'_> {
    /// Ties message `next` to its two blocks; returns the digest as 32
    /// range-checked byte cells.
    fn hash(
        &mut self,
        ctx: &mut Context<Fr>,
        range: &RangeChip<Fr>,
        msg: &[AssignedValue<Fr>],
    ) -> Vec<AssignedValue<Fr>> {
        assert_eq!(msg.len(), 64);
        let gate = range.gate();
        let (first, last) = (&self.blocks[2 * self.next], &self.blocks[2 * self.next + 1]);
        let digest = sha(&self.msgs[self.next]);
        self.next += 1;

        gate.assert_is_const(ctx, &first.is_final, &Fr::ZERO);
        gate.assert_is_const(ctx, &last.is_final, &Fr::ONE);
        gate.assert_is_const(ctx, &last.length, &Fr::from(64));
        for (word, bytes) in first.words.iter().zip(msg.chunks(4)) {
            let packed = le_int(ctx, gate, bytes);
            ctx.constrain_equal(word, &packed);
        }
        let out = load_checked_bytes(ctx, range, &digest);
        let hi = be_int(ctx, gate, &out[0..16]);
        let lo = be_int(ctx, gate, &out[16..32]);
        ctx.constrain_equal(&last.hash_hi, &hi);
        ctx.constrain_equal(&last.hash_lo, &lo);
        out
    }
}

fn load_checked_bytes(
    ctx: &mut Context<Fr>,
    range: &RangeChip<Fr>,
    bytes: &[u8],
) -> Vec<AssignedValue<Fr>> {
    bytes
        .iter()
        .map(|&v| {
            let c = ctx.load_witness(Fr::from(v as u64));
            range.range_check(ctx, c, 8);
            c
        })
        .collect()
}

/// `Σ b_i · 256^i`.
fn le_int(
    ctx: &mut Context<Fr>,
    gate: &impl GateInstructions<Fr>,
    bytes: &[AssignedValue<Fr>],
) -> AssignedValue<Fr> {
    let weights = (0..bytes.len()).map(|i| QuantumCell::Constant(Fr::from(256).pow([i as u64])));
    gate.inner_product(
        ctx,
        bytes.iter().copied().map(QuantumCell::Existing),
        weights,
    )
}

/// `Σ b_i · 256^(n-1-i)`.
fn be_int(
    ctx: &mut Context<Fr>,
    gate: &impl GateInstructions<Fr>,
    bytes: &[AssignedValue<Fr>],
) -> AssignedValue<Fr> {
    let reversed: Vec<_> = bytes.iter().rev().copied().collect();
    le_int(ctx, gate, &reversed)
}

/// `use_b ? b : a` for two constant booleans, as a cell.
fn select_const(
    ctx: &mut Context<Fr>,
    use_b: AssignedValue<Fr>,
    use_a: AssignedValue<Fr>,
    b: bool,
    a: bool,
) -> AssignedValue<Fr> {
    match (b, a) {
        (true, true) => ctx.load_constant(Fr::ONE),
        (false, false) => ctx.load_zero(),
        (true, false) => use_b,
        (false, true) => use_a,
    }
}

/// The instances the circuit exposes for `w`, computed natively.
pub fn native_instances(w: &CommitteeWitness) -> Vec<Fr> {
    let le = |b: &[u8]| {
        b.iter()
            .rev()
            .fold(Fr::ZERO, |acc, &x| acc * Fr::from(256) + Fr::from(x as u64))
    };
    vec![
        native_committee_commitment(&w.pubkeys, &w.aggregate),
        le(&w.state_root[0..16]),
        le(&w.state_root[16..32]),
        Fr::from(w.attested_slot),
        Fr::from(w.gloas_fork_slot),
    ]
}

/// [`crate::rotate::commit_sync_committee_2level`] outside a circuit, through
/// the same chips on a throwaway builder.
pub fn native_committee_commitment(pubkeys: &[[u8; 48]], aggregate: &[u8; 48]) -> Fr {
    let mut b = BaseCircuitBuilder::<Fr>::new(false);
    let gate = halo2_base::gates::GateChip::<Fr>::default();
    let ctx = b.main(0);
    let flat: Vec<u8> = pubkeys.iter().flatten().copied().collect();
    let pk = crate::ssz::load_bytes(ctx, &flat);
    let agg = crate::ssz::load_bytes(ctx, aggregate);
    let hasher = new_committee_hasher(ctx, &gate);
    let digests: Vec<_> = pk
        .chunks(PUBKEYS_PER_SHARD * 48)
        .map(|slice| commit_committee_shard(&hasher, ctx, &gate, slice))
        .collect();
    let out = *combine_committee_commitment(&hasher, ctx, &gate, &digests, &agg).value();
    b.clear();
    out
}

impl Circuit<Fr> for CommitteeCircuit {
    type Config = CommitteeCircuitConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = CommitteeCircuitParams;

    fn params(&self) -> Self::Params {
        self.params.clone()
    }

    fn without_witnesses(&self) -> Self {
        unimplemented!()
    }

    fn configure_with_params(
        meta: &mut ConstraintSystem<Fr>,
        params: Self::Params,
    ) -> Self::Config {
        let sha256 = Sha256CircuitConfig::new(meta);
        // Last: it reads the unusable rows of everything configured before it.
        let base = BaseCircuitBuilder::configure_with_params(meta, params.base);
        CommitteeCircuitConfig {
            base,
            sha256,
        }
    }

    fn configure(_: &mut ConstraintSystem<Fr>) -> Self::Config {
        unreachable!("configured with params")
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        let msgs = self
            .sha_inputs
            .clone()
            .unwrap_or_else(|| self.witness.messages().0);
        let inputs: Vec<Vec<u8>> = msgs.iter().map(|m| m.to_vec()).collect();
        let mut assigned = Vec::new();
        layouter.assign_region(
            || "sha256",
            |mut region| {
                assigned =
                    config
                        .sha256
                        .multi_sha256(&mut region, inputs.clone(), Some(NUM_BLOCKS));
                Ok(())
            },
        )?;
        let blocks = self.load_blocks(assigned);
        self.constrain(&blocks);
        self.builder.borrow().synthesize(config.base, layouter)?;
        self.builder.borrow_mut().clear();
        Ok(())
    }
}

#[cfg(feature = "aggregation")]
impl snark_verifier_sdk::CircuitExt<Fr> for CommitteeCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![COMMITTEE_INSTANCE_LEN]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        vec![self.instances.clone()]
    }
}

/// Rows the table circuit uses; the rest of `2^k` is the base part's.
pub const fn sha256_rows() -> usize {
    NUM_BLOCKS * SHA256_NUM_ROWS
}

const _: () = assert!(sha256_rows() + NUM_UNUSABLE_ROWS <= 1 << COMMITTEE_TABLE_K);
