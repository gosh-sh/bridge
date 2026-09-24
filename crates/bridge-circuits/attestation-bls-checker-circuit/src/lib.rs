pub mod attestation_data_parser;
pub mod primary_circuit;
pub mod fallback_circuit;

#[cfg(test)]
mod primary_circuit_profile;

pub mod test_instances;

use halo2_base::utils::{BigPrimeField, ScalarField};
use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        GateInstructions, RangeChip, RangeInstructions,
    },
    halo2_proofs::plonk::{Circuit, ConstraintSystem},
    AssignedValue, QuantumCell,
};

/// Byte offset of the `block_id` hash within bincode-serialized `AttestationData`.
/// = parent_block_id(40) + block_id_length_prefix(8) = 48
/// BlockIdentifier is serialized as 8-byte u64 LE length (=32) + 32 bytes.
pub const BLOCK_ID_REL_OFFSET: usize = 48;

/// Byte offset of the `block_seq_no` field within bincode-serialized `AttestationData`.
/// = parent_block_id(40) + block_id(40) = 80
/// Bincode serializes `BlockSeqNo(u32)` as 4 bytes little-endian.
pub const BLOCK_SEQ_NO_REL_OFFSET: usize = 80;

/// Byte offset of the `target_type` field within bincode-serialized `AttestationData`.
/// = parent_block_id(40) + block_id(40) + block_seq_no(4) + envelope_hash(32) = 116
/// Bincode serializes the `#[repr(u8)]` enum as a u32 LE discriminant:
/// Primary = 0x00000000, Fallback = 0x01000000.
pub const TARGET_TYPE_REL_OFFSET: usize = 116;

/// Expected byte length of bincode 1.x–serialized AttestationData.
/// = BlockIdentifier(40) + BlockIdentifier(40) + BlockSeqNo(4)
///   + AckiNackiEnvelopeHash(32) + AttestationTargetType(4) = 120
pub const ATTESTATION_DATA_LEN: usize = 120;

// ---------------------------------------------------------------------------
// Circuit parameters
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct AttestationBlsCheckerCircuitParams {
    pub k: usize,
    pub num_unusable_rows: usize,
    pub base_circuit_params: BaseCircuitParams,
    pub limb_bits: usize,
    pub num_limbs: usize,
    pub max_signers: usize,
}

// ---------------------------------------------------------------------------
// Circuit config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AttestationBlsCheckerConfig<F: ScalarField> {
    pub(crate) base_config: BaseConfig<F>,
}

impl<F: ScalarField> AttestationBlsCheckerConfig<F> {
    pub fn configure_with_params(
        meta: &mut ConstraintSystem<F>,
        params: BaseCircuitParams,
    ) -> Self {
        let base_config =
            <BaseCircuitBuilder<F> as Circuit<F>>::configure_with_params(meta, params);
        AttestationBlsCheckerConfig { base_config }
    }
}

// ---------------------------------------------------------------------------
// Shared constraint: block_seq_no > last_seen_block_seqno
// ---------------------------------------------------------------------------

/// Constrain block_seq_no (from attestation) > last_seen_block_seqno.
///
/// Extracts block_seq_no from assigned attestation data bytes, loads
/// last_seen_block_seqno as a witness, and proves the strict inequality
/// via range-checking `(block_seq_no - last_seen - 1)` fits in 32 bits.
///
/// Returns `(block_seq_no_fr, last_seen_assigned)` for public instance exposure.
pub(crate) fn constraint_block_seqno_gt_last_seen<F: BigPrimeField>(
    builder: &mut BaseCircuitBuilder<F>,
    range: &RangeChip<F>,
    assigned_msg: &[AssignedValue<F>],
    last_seen_block_seqno: u32,
) -> (AssignedValue<F>, AssignedValue<F>) {
    let ctx = builder.main(0);
    let gate = range.gate();

    // Extract block_seq_no (4 bytes LE at BLOCK_SEQ_NO_REL_OFFSET).
    let seqno_cells =
        &assigned_msg[BLOCK_SEQ_NO_REL_OFFSET..BLOCK_SEQ_NO_REL_OFFSET + 4];
    let block_seq_no_fr = gate.inner_product(
        ctx,
        seqno_cells.iter().map(|&b| QuantumCell::Existing(b)),
        (0..4).map(|i| QuantumCell::Constant(F::from(256u64).pow([i as u64]))),
    );

    // Load last_seen_block_seqno as witness (exposed as public instance by caller).
    let last_seen = ctx.load_witness(F::from(last_seen_block_seqno as u64));

    // Prove block_seq_no > last_seen_block_seqno:
    // (block_seq_no - last_seen - 1) must fit in 32 bits, i.e. be in [0, 2^32).
    let diff = gate.sub(ctx, block_seq_no_fr, last_seen);
    let one = ctx.load_constant(F::from(1u64));
    let diff_minus_one = gate.sub(ctx, diff, one);
    range.range_check(ctx, diff_minus_one, 32);

    (block_seq_no_fr, last_seen)
}

// ---------------------------------------------------------------------------
// Shared test parameters
// ---------------------------------------------------------------------------
//
// Used by the in-source `tests` modules in `primary_circuit.rs` /
// `fallback_circuit.rs`, the integration tests under `tests/`, and
// cross-circuit tests in sibling crates. BK-set Poseidon parameters
// (`LIMB_BITS`, `NUM_LIMBS`, `MAX_SIGNERS`) live in `bridge_poseidon`;
// the native commitment is `bridge_poseidon::compute_bk_set_poseidon`.

pub const K: u32 = 20;
pub const NUM_UNUSABLE_ROWS: usize = 109;
pub const LOOKUP_BITS: usize = 19;
