//! Fiat–Shamir transcript flavours supported by the bridge provers.
//!
//! * [`kind::TranscriptKind`] — the discriminator threaded through
//!   `generate_*_proof_with_transcript` variants (see
//!   [`crate::prover`], [`crate::verifier`], [`crate::layer_prover`], and
//!   `bridge_event_prover_lib::prover`).
//! * [`poseidon`] — native Poseidon transcript that produces the SAME proof
//!   bytes as `snark-verifier-sdk`'s `PoseidonTranscript<NativeLoader, _>`.
//!   This is what lets a bridge SNARK produced here be fed unchanged into a
//!   downstream `AggregationCircuit` living in a workspace pinned to
//!   axiom-crypto's `halo2-lib`.
//!
//! The Blake2b transcript is provided directly by the halo2 crate
//! (`halo2_proofs::transcript::Blake2bWrite/Read`) — no wrapper needed.

pub mod kind;
pub mod poseidon;

pub use kind::TranscriptKind;
pub use poseidon::{PoseidonChallenge, PoseidonRead, PoseidonWrite};
