//! `eth-light-client-prover` — trustless Ethereum sync-committee light client
//! circuit for the ETH→AN deposit path.
//!
//! Milestones (see `../docs/eth_light_client_trustless_deposit_plan.md`):
//! - **M0** — frozen spec + fixtures (`docs/m0_spec.md`).
//! - **M1** — BLS core (this): committee select + 2/3 supermajority + G1
//!   aggregate + BLS12-381 pairing + RFC-9380 hash-to-curve. See [`bls_core`]
//!   and `docs/m1_notes.md`.
//! - M2+ — SSZ Merkle branches, full update circuit, VkBlob, AN contract, relayer.

pub mod bls_core;
pub mod committee;
pub mod decode;
pub mod execution;
pub mod mainnet;
pub mod poseidon_transcript;
pub mod rotate;
#[cfg(feature = "aggregation")]
pub mod rotate_aggregation;
pub mod signing;
pub mod ssz;
pub mod step;
pub mod subgroup;

pub use bls_core::{
    ETH_BLS_DST, LIMB_BITS, NUM_LIMBS, SYNC_COMMITTEE_SIZE, SYNC_COMMITTEE_THRESHOLD,
};
