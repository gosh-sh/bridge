//! Electra/Fulu ↔ Gloas: the generalized indices the light-client proofs use, and
//! the in-circuit test that picks between them.
//!
//! Gloas (EIP-7732 ePBS + EIP-7688 progressive containers) moved every index the
//! step and rotate circuits prove (`consensus-specs/specs/gloas/light-client/
//! sync-protocol.md`, *Constants*), and replaced the light-client header's
//! execution payload with the bid's `parent_block_hash`. A pre-Gloas header is
//! carried in the Gloas shape (`upgrade_lc_header_to_gloas`): its execution
//! block hash is proven straight from `body_root` at
//! [`EXECUTION_BLOCK_HASH_GINDEX_DENEB`], with no payload header in the witness.
//!
//! Which index applies is decided per header from its slot against the network's
//! Gloas fork slot, the way the spec's `*_gindex_at_slot` helpers do. The fork
//! slot is a **public input**, not a constant, so one verification key serves every
//! network; the consumer pins it to the network's value.

use halo2_base::gates::{GateInstructions, RangeChip, RangeInstructions};
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, Context};

/// `finalized_checkpoint.root` in `BeaconState`, Electra..Fulu. Depth 7.
pub const FINALIZED_ROOT_GINDEX_ELECTRA: u64 = 169;
/// `finalized_checkpoint.root` in `BeaconState`, Gloas. Depth 9.
pub const FINALIZED_ROOT_GINDEX_GLOAS: u64 = 735;
/// Nodes in a finality branch the step circuit takes (the deeper of the two).
pub const FINALITY_BRANCH_LEN: usize = 9;

/// `current_sync_committee` in `BeaconState`, Electra..Fulu. Depth 6.
pub const CURRENT_SYNC_COMMITTEE_GINDEX_ELECTRA: u64 = 86;
/// `current_sync_committee` in `BeaconState`, Gloas. Depth 11.
pub const CURRENT_SYNC_COMMITTEE_GINDEX_GLOAS: u64 = 2945;
/// `next_sync_committee` in `BeaconState`, Electra..Fulu. Depth 6.
pub const NEXT_SYNC_COMMITTEE_GINDEX_ELECTRA: u64 = 87;
/// `next_sync_committee` in `BeaconState`, Gloas. Depth 11.
pub const NEXT_SYNC_COMMITTEE_GINDEX_GLOAS: u64 = 2946;
/// Nodes in a sync-committee branch the rotate circuit takes.
pub const SYNC_COMMITTEE_BRANCH_LEN: usize = 11;

/// `execution_payload.block_hash` in `BeaconBlockBody`, Deneb..Fulu. Depth 9:
/// 5 levels inside `ExecutionPayloadHeader` + the 4-node `execution_branch`.
pub const EXECUTION_BLOCK_HASH_GINDEX_DENEB: u64 = 812;
/// `signed_execution_payload_bid.message.parent_block_hash` in `BeaconBlockBody`,
/// Gloas. Depth 11.
pub const EXECUTION_BLOCK_HASH_GINDEX_GLOAS: u64 = 2856;
/// Nodes in an execution branch the step circuit takes.
pub const EXECUTION_BRANCH_LEN: usize = 11;

/// Gloas fork slot of a network whose Gloas fork is not scheduled
/// (`FAR_FUTURE_EPOCH`): every slot is pre-Gloas.
pub const GLOAS_NOT_SCHEDULED: u64 = u64::MAX;
/// Sepolia: Gloas at epoch 353024 (2026-10-06 13:53:36 UTC).
pub const SEPOLIA_GLOAS_FORK_SLOT: u64 = 353_024 * 32;

/// The Gloas fork slot of a network from its fork epoch; `FAR_FUTURE_EPOCH` and
/// any epoch whose first slot does not fit a `u64` map to [`GLOAS_NOT_SCHEDULED`].
pub fn gloas_fork_slot(fork_epoch: u64, slots_per_epoch: u64) -> u64 {
    fork_epoch.checked_mul(slots_per_epoch).unwrap_or(GLOAS_NOT_SCHEDULED)
}

/// Whether `slot` is at or after the Gloas fork.
pub fn is_gloas(slot: u64, gloas_fork_slot: u64) -> bool {
    slot >= gloas_fork_slot
}

/// In-circuit [`is_gloas`]: a boolean cell, `1` iff `slot >= fork_slot`. Both
/// inputs are range-checked to 64 bits here, so the comparison is exact.
pub fn assert_is_gloas<F: BigPrimeField>(
    ctx: &mut Context<F>,
    range: &RangeChip<F>,
    slot: AssignedValue<F>,
    fork_slot: AssignedValue<F>,
) -> AssignedValue<F> {
    range.range_check(ctx, slot, 64);
    range.range_check(ctx, fork_slot, 64);
    let before = range.is_less_than(ctx, slot, fork_slot, 64);
    range.gate().not(ctx, before)
}
