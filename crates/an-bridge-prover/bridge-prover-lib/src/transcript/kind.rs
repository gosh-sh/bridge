//! `TranscriptKind` — the Fiat–Shamir transcript discriminator threaded
//! through every `_with_transcript` prover/verifier entry point.
//!
//! Wire encoding: this enum is `#[repr(u8)]` because it is serialised
//! into on-wire structures (notably the `VkBlob` header in
//! `bridge-prover-orchestrator::halo2_tvm_bundle`). The numeric values
//! MUST stay stable across releases:
//!
//! * `0` = Blake2b — the AN-side default and the only variant accepted by
//!   the `ZKHALO2VERIFYWITHVK` opcode.
//! * `1` = reserved for Keccak (`EvmTranscript`) — not yet implemented.
//! * `2` = Poseidon — added 2026-05-27 for the R15 ETH-side aggregator
//!   pipeline. Inner SNARKs fed to `snark-verifier-sdk::AggregationCircuit`
//!   MUST use this. Rejected by the AN opcode.

use anyhow::{anyhow, Result};

/// Fiat–Shamir transcript flavour used to derive verifier challenges.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    Blake2b = 0,
    // 1 is reserved for Keccak (EvmTranscript).
    Poseidon = 2,
}

impl TranscriptKind {
    /// Parse from the on-wire discriminator byte.
    pub fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Blake2b),
            2 => Ok(Self::Poseidon),
            other => Err(anyhow!(
                "unknown transcript_kind byte {other} (defined: 0 = Blake2b, 2 = Poseidon)"
            )),
        }
    }

    /// The on-wire discriminator byte.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}
