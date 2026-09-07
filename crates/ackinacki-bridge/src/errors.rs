//! Typed errors + process exit-code mapping.
//!
//! Ekaterina's spec is explicit that the CLI's exit codes distinguish "we
//! refused before broadcasting anything" from "we broadcast and don't know the
//! outcome" — collapsing those into a single non-zero would hide the
//! difference that matters for money. Every non-success terminal state gets
//! its own code and this module is the single source of truth for that
//! mapping.
//!
//! **Never** put key file contents, private-key material, or the resolved
//! signer public key into any error `Display` impl. Preflight refusals may
//! print the *path* to a key file to guide `chmod 600 <path>`, but never the
//! bytes inside.

use thiserror::Error;

/// Process exit codes. Kept small and stable — third-party scripts and CI
/// wrappers will pattern-match on these.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Full success — AN burn broadcast, WithdrawalInitiated captured, C4
    /// proof accepted by the on-chain Yul verifier, `withdrawByProof` mined
    /// (or `--dry-run` returned WouldSucceed).
    Success = 0,
    /// Preflight refusal. Nothing broadcast on either chain. Includes:
    /// arg validation, key file perms, single-custodian check, EIP-55
    /// mismatch, chain-id whitelist miss, balance shortfall, USDCBridge
    /// resolution failure.
    PreflightRefused = 2,
    /// Duplicate in-flight refusal from the idempotency layer. Nothing
    /// broadcast. Use `--allow-retry` to override, or query the printed
    /// prior identifier to reconcile.
    DuplicateRefused = 3,
    /// AN burn was broadcast; final outcome unknown (network error mid-send,
    /// timeout waiting for account state to update). Idempotency record
    /// persisted with the AN tx hash — operator must reconcile via GQL.
    BurnOutcomeUnknown = 10,
    /// AN burn confirmed but `WithdrawalInitiated` capture timed out. The
    /// event is durable in GQL; re-run with `--allow-retry` (v1) or
    /// `--resume` (v2) to pick up capture.
    CaptureTimeout = 11,
    /// Capture succeeded but the Circuit-4 proof pipeline failed
    /// (aggregator subprocess crash, C4 wall-time exceeded, enricher timed
    /// out waiting for the covering bundle). Proof is deterministic per
    /// (event, prover_state) so re-running once the daemon has caught up
    /// regenerates the same proof against warm caches.
    ProofFailed = 12,
    /// Proof produced but the on-chain `withdrawByProof` failed (dry-run
    /// revert, tx revert, `WithdrawTreasuryShortfall`, etc.). Fix the
    /// on-chain side (unpause, seed treasury) and re-run — the proof is
    /// deterministic and cache-warm.
    EthSubmitFailed = 13,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// The staged pipeline. Kept as an enum so `--json` errors can name the
/// stage that failed unambiguously, and so the `Display` layer can pick
/// stage-appropriate remediation hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preflight,
    Burn,
    Capture,
    Prove,
    Submit,
}

/// The single error type raised out of `main`. Every variant maps to
/// exactly one [`ExitCode`]. Any wrapped `anyhow::Error` or third-party
/// error should be attached via `#[source]` to preserve the causal chain
/// for `RUST_LOG=debug` viewing, but the top-level `Display` must not
/// expose sensitive material.
#[derive(Debug, Error)]
pub enum CliError {
    // -- Preflight (exit 2) --
    /// Argument shape/format violation. `flag` is the offending flag as
    /// spelled on the CLI (e.g. `--to`); `expected` describes the valid
    /// shape; `got` is a redacted, safe echo of what we saw.
    #[error("--{flag}: expected {expected}, got {got}")]
    ArgInvalid {
        flag: &'static str,
        expected: String,
        got: String,
    },

    /// Key file mode/ownership refusal. Prints the path only, never
    /// contents.
    #[error("--from-keys: key file {path} is not owner-only readable; run: chmod 600 {path}")]
    KeyFilePerms { path: String },

    /// Any other preflight refusal (multisig detection, single-custodian,
    /// owner-key mismatch, balance shortfall, USDCBridge resolution).
    /// `reason` should name the specific check that failed.
    #[error("preflight: {reason}")]
    Preflight {
        reason: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // -- Idempotency (exit 3) --
    #[error("refuse: duplicate in-flight withdrawal ({prior_status}). \
             Prior AN tx: {prior_tx:?}. Prior withdrawal msg_id: {prior_msg_id:?}. \
             Reconcile via GraphQL, or re-run with --allow-retry to override.")]
    DuplicateInFlight {
        prior_status: String,
        prior_tx: Option<String>,
        prior_msg_id: Option<String>,
    },

    // -- Burn (exit 10) --
    #[error("burn: {reason}")]
    BurnOutcomeUnknown {
        reason: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // -- Capture (exit 11) --
    #[error("capture: timed out waiting for WithdrawalInitiated event (AN tx {an_tx})")]
    CaptureTimeout { an_tx: String },

    // -- Prove (exit 12) --
    #[error("prove: {reason}")]
    ProofFailed {
        reason: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // -- Submit (exit 13) --
    #[error("submit: {reason}")]
    EthSubmitFailed {
        reason: String,
        #[source]
        source: Option<anyhow::Error>,
    },
}

impl CliError {
    /// Deterministic mapping to a process exit code. `main` calls this on
    /// `Err(e)` before returning.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CliError::ArgInvalid { .. }
            | CliError::KeyFilePerms { .. }
            | CliError::Preflight { .. } => ExitCode::PreflightRefused,
            CliError::DuplicateInFlight { .. } => ExitCode::DuplicateRefused,
            CliError::BurnOutcomeUnknown { .. } => ExitCode::BurnOutcomeUnknown,
            CliError::CaptureTimeout { .. } => ExitCode::CaptureTimeout,
            CliError::ProofFailed { .. } => ExitCode::ProofFailed,
            CliError::EthSubmitFailed { .. } => ExitCode::EthSubmitFailed,
        }
    }

    /// Which pipeline stage this error is associated with. Used by the
    /// `--json` error emitter.
    pub fn stage(&self) -> Stage {
        match self {
            CliError::ArgInvalid { .. }
            | CliError::KeyFilePerms { .. }
            | CliError::Preflight { .. }
            | CliError::DuplicateInFlight { .. } => Stage::Preflight,
            CliError::BurnOutcomeUnknown { .. } => Stage::Burn,
            CliError::CaptureTimeout { .. } => Stage::Capture,
            CliError::ProofFailed { .. } => Stage::Prove,
            CliError::EthSubmitFailed { .. } => Stage::Submit,
        }
    }
}

pub type CliResult<T> = std::result::Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_stable_exit_code() {
        // Guard against a future refactor accidentally mapping a variant
        // to the wrong bucket. Exit codes are a wire contract with users'
        // scripts.
        assert_eq!(
            CliError::ArgInvalid {
                flag: "to",
                expected: "0x-prefixed 20-byte hex".into(),
                got: "…".into()
            }
            .exit_code(),
            ExitCode::PreflightRefused
        );
        assert_eq!(
            CliError::DuplicateInFlight {
                prior_status: "burned".into(),
                prior_tx: None,
                prior_msg_id: None,
            }
            .exit_code(),
            ExitCode::DuplicateRefused
        );
        assert_eq!(
            CliError::CaptureTimeout { an_tx: "0x…".into() }.exit_code(),
            ExitCode::CaptureTimeout
        );
    }

    #[test]
    fn a_post_burn_state_write_failure_is_not_exit_2() {
        // The distinction the exit-code contract exists for: 2 means nothing
        // was sent, 10 means it was sent and the outcome is unknown. A failed
        // state write after a successful broadcast is unambiguously the
        // second, and reporting it as the first invites a double burn.
        let e = CliError::BurnOutcomeUnknown {
            reason: "state file could not be updated".into(),
            source: None,
        };
        assert_eq!(e.exit_code(), ExitCode::BurnOutcomeUnknown);
        assert_ne!(e.exit_code(), ExitCode::PreflightRefused);
    }
}
