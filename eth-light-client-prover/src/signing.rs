//! M2 — beacon `signing_root` derivation (the message M1 verifies the committee
//! signed): `BeaconBlockHeader` htr → `ForkData`/`domain` → `SigningData` root.
//!
//! `signing_root = SHA256( hash_tree_root(header.beacon) ‖ domain )`, where
//! `domain = DOMAIN_SYNC_COMMITTEE ‖ fork_data_root[:28]` and
//! `fork_data_root = hash_tree_root(ForkData{ fork_version(signature_slot), genesis_validators_root })`.
//!
//! All in-circuit (real SHA-256), so the message point M1 feeds to
//! `hash_to_curve` is provably the beacon signing root — no trusted precompute.

use crate::ssz::{
    bytes_root, container_root, native_bytes_root, native_container_root, native_sha256_pair,
    native_uint64_root, sha256_pair, uint64_root, Node,
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::utils::BigPrimeField;
use halo2_base::{AssignedValue, Context};

/// Sync-committee signature domain type (consensus-specs `DOMAIN_SYNC_COMMITTEE`).
pub const DOMAIN_SYNC_COMMITTEE: [u8; 4] = [0x07, 0x00, 0x00, 0x00];

/// Mainnet `genesis_validators_root`
/// (`0x4b363db94e286120d76eb905340fdd4e54bfe9f06bf33ff6cf5ad27f511bfe95`).
pub const MAINNET_GENESIS_VALIDATORS_ROOT: [u8; 32] = [
    0x4b, 0x36, 0x3d, 0xb9, 0x4e, 0x28, 0x61, 0x20, 0xd7, 0x6e, 0xb9, 0x05, 0x34, 0x0f, 0xdd, 0x4e,
    0x54, 0xbf, 0xe9, 0xf0, 0x6b, 0xf3, 0x3f, 0xf6, 0xcf, 0x5a, 0xd2, 0x7f, 0x51, 0x1b, 0xfe, 0x95,
];

// Mainnet fork versions (see m0_spec §5).
pub const FORK_VERSION_ALTAIR: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
pub const FORK_VERSION_BELLATRIX: [u8; 4] = [0x02, 0x00, 0x00, 0x00];
pub const FORK_VERSION_CAPELLA: [u8; 4] = [0x03, 0x00, 0x00, 0x00];
pub const FORK_VERSION_DENEB: [u8; 4] = [0x04, 0x00, 0x00, 0x00];
pub const FORK_VERSION_ELECTRA: [u8; 4] = [0x05, 0x00, 0x00, 0x00];
pub const FORK_VERSION_FULU: [u8; 4] = [0x06, 0x00, 0x00, 0x00];

// ---------------------------------------------------------------------------
// In-circuit
// ---------------------------------------------------------------------------

/// `hash_tree_root(BeaconBlockHeader)` — 5 fields (slot, proposer_index,
/// parent_root, state_root, body_root), padded to 8.
#[allow(clippy::too_many_arguments)]
pub fn beacon_header_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    slot_le: &[AssignedValue<F>],
    proposer_le: &[AssignedValue<F>],
    parent_root: &Node<F>,
    state_root: &Node<F>,
    body_root: &Node<F>,
) -> Node<F> {
    let slot_r = uint64_root(ctx, slot_le);
    let proposer_r = uint64_root(ctx, proposer_le);
    container_root(
        chip,
        ctx,
        vec![slot_r, proposer_r, parent_root.clone(), state_root.clone(), body_root.clone()],
    )
}

/// `fork_data_root = hash_tree_root(ForkData{ current_version, genesis_validators_root })`.
pub fn fork_data_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    fork_version: &[AssignedValue<F>],
    genesis_validators_root: &Node<F>,
) -> Node<F> {
    let version_root = bytes_root(chip, ctx, fork_version);
    container_root(chip, ctx, vec![version_root, genesis_validators_root.clone()])
}

/// `domain = DOMAIN_SYNC_COMMITTEE ‖ fork_data_root[:28]`.
pub fn compute_domain<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    fork_version: &[AssignedValue<F>],
    genesis_validators_root: &Node<F>,
) -> Node<F> {
    let fdr = fork_data_root(chip, ctx, fork_version, genesis_validators_root);
    let mut node: Node<F> =
        DOMAIN_SYNC_COMMITTEE.iter().map(|&b| ctx.load_constant(F::from(b as u64))).collect();
    node.extend_from_slice(&fdr[0..28]);
    node
}

/// `signing_root = SHA256( object_root ‖ domain )`.
pub fn signing_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    object_root: &Node<F>,
    domain: &Node<F>,
) -> Node<F> {
    sha256_pair(chip, ctx, object_root, domain)
}

/// End-to-end: beacon header root → domain → signing root.
#[allow(clippy::too_many_arguments)]
pub fn sync_committee_signing_root<F: BigPrimeField>(
    chip: &Sha256Chip<F>,
    ctx: &mut Context<F>,
    slot_le: &[AssignedValue<F>],
    proposer_le: &[AssignedValue<F>],
    parent_root: &Node<F>,
    state_root: &Node<F>,
    body_root: &Node<F>,
    fork_version: &[AssignedValue<F>],
    genesis_validators_root: &Node<F>,
) -> Node<F> {
    let object_root =
        beacon_header_root(chip, ctx, slot_le, proposer_le, parent_root, state_root, body_root);
    let domain = compute_domain(chip, ctx, fork_version, genesis_validators_root);
    signing_root(chip, ctx, &object_root, &domain)
}

// ---------------------------------------------------------------------------
// Native references
// ---------------------------------------------------------------------------

pub fn native_beacon_header_root(
    slot: u64,
    proposer_index: u64,
    parent_root: &[u8; 32],
    state_root: &[u8; 32],
    body_root: &[u8; 32],
) -> [u8; 32] {
    native_container_root(vec![
        native_uint64_root(slot),
        native_uint64_root(proposer_index),
        *parent_root,
        *state_root,
        *body_root,
    ])
}

pub fn native_fork_data_root(fork_version: &[u8; 4], genesis_validators_root: &[u8; 32]) -> [u8; 32] {
    native_container_root(vec![native_bytes_root(fork_version), *genesis_validators_root])
}

pub fn native_compute_domain(fork_version: &[u8; 4], genesis_validators_root: &[u8; 32]) -> [u8; 32] {
    let fdr = native_fork_data_root(fork_version, genesis_validators_root);
    let mut domain = [0u8; 32];
    domain[..4].copy_from_slice(&DOMAIN_SYNC_COMMITTEE);
    domain[4..32].copy_from_slice(&fdr[..28]);
    domain
}

pub fn native_signing_root(object_root: &[u8; 32], domain: &[u8; 32]) -> [u8; 32] {
    native_sha256_pair(object_root, domain)
}

#[allow(clippy::too_many_arguments)]
pub fn native_sync_committee_signing_root(
    slot: u64,
    proposer_index: u64,
    parent_root: &[u8; 32],
    state_root: &[u8; 32],
    body_root: &[u8; 32],
    fork_version: &[u8; 4],
    genesis_validators_root: &[u8; 32],
) -> [u8; 32] {
    let object_root =
        native_beacon_header_root(slot, proposer_index, parent_root, state_root, body_root);
    let domain = native_compute_domain(fork_version, genesis_validators_root);
    native_signing_root(&object_root, &domain)
}
