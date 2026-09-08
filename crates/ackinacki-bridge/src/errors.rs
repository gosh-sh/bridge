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
//! print the *path* to a key file to guide `chmod 400 <path>`, but never the
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
    /// broadcast.
    ///
    /// Two refusals share this code and they take DIFFERENT remedies —
    /// this doc used to give only the first, which is the advice five
    /// other places in the tree call a defect:
    ///
    /// * [`CliError::DuplicateInFlight`] — the record names an AN tx hash.
    ///   `--allow-retry` resumes at capture for the active statuses, and is
    ///   refused outright for `confirmed` and `submitted`; the refusal carries
    ///   the per-status remedy.
    /// * [`CliError::ReservationInFlight`] — the record has NO hash, so nothing
    ///   on disk can say whether a burn is on the wire. `--allow-retry` does
    ///   not override it and the message says so. The remedy is on-chain
    ///   reconciliation, and then either writing the hash in or deleting the
    ///   record — the second only under the liveness verdict that permits it.
    ///
    /// Read the refusal, not this list: both print what to do.
    DuplicateRefused = 3,
    /// AN burn was broadcast; final outcome unknown (network error
    /// mid-send, timeout waiting for account state to update).
    ///
    /// **The record usually does NOT carry the hash.** This doc claimed
    /// the opposite, which is the reverse of the dominant case: every
    /// failure inside `burn::send` propagates before the block that
    /// writes `an_tx_hash`, so what an exit 10 leaves is a `Reserved`
    /// record with `an_tx_hash: null` — and the CLI cannot write a hash
    /// it never learned. The hash is on the record only for the two exit
    /// 10s raised AFTER the send returned: a failed post-burn `update`,
    /// and a refusal on the resume path.
    ///
    /// So reconciliation is the remedy in every case, and where the hash
    /// is not on the record it is not in the CLI either: look for a
    /// `sendTransaction` from this multisig around the record's
    /// `reserved_at`. The advanced runbook's Case 3a is the procedure.
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

    /// Key file rejection. `problem` names which check failed so the
    /// operator gets the right remedy; `path` is echoed to make the
    /// remedy copy-pasteable. Contents are NEVER read here, let alone
    /// printed.
    #[error("--from-keys: {path}: {problem}")]
    KeyFilePerms { path: String, problem: String },

    /// Invocation could not be parsed, or `$BRIDGE_CONFIG` could not be
    /// loaded, or the async runtime would not start. Nothing ran, nothing
    /// was broadcast — hence exit 2, the same code every other pre-send
    /// refusal uses. `reason` carries clap's own rendered message so the
    /// human-readable path loses nothing.
    #[error("{reason}")]
    Usage { reason: String },

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
    //
    // Two situations, two messages, and the split is the PRIOR RECORD'S
    // HASH — not which stage noticed, and not which flags were passed.
    // This comment said otherwise for two rounds and the description it
    // gave was inverted in both halves; the wrong half is what kept
    // sending operators at the guard itself.
    //
    // `DuplicateInFlight` is for a record that carries an AN tx hash.
    // `reserve` is its only source. Two shapes: a resumable status, where
    // "re-run with --allow-retry" genuinely resumes at capture, and a
    // terminal one (`confirmed`, `submitted`), where the flag changes
    // nothing and the remedy says so instead — see
    // `idempotency::terminal_refusal`.
    //
    // `ReservationInFlight` is for a record with NO hash, which cannot say
    // whether a burn is on the wire, because the hash is written only
    // after the send returns. Raised in THREE places — stage 1 before
    // reserving, the contended-lock arm of `reserve_and_decide_holding`,
    // and `decide_burn` after the reservation. (This said two for several
    // rounds; `the_hash_less_refusal_is_raised_from_exactly_three_places`
    // is what keeps the count honest now.) Independent of
    // `--allow-retry` in both: the flag does not override it and the text
    // says so. What it carries instead is the verdict `flock` can give
    // (`idempotency::liveness_verdict`) and the record's path, because
    // deleting that record is the real remedy and doing it while another
    // run is mid-send is the second burn this whole refusal exists to
    // prevent.
    //
    // Naming `--allow-retry` from the hash-less case is therefore always
    // wrong: it routes an operator to a message whose own text is
    // "--allow-retry does NOT override this".
    #[error(
        "refuse: duplicate in-flight withdrawal ({prior_status}). Prior AN tx: {prior_tx:?}. \
         Prior withdrawal msg_id: {prior_msg_id:?}.\n\x20 {remedy}"
    )]
    DuplicateInFlight {
        prior_status: String,
        prior_tx: Option<String>,
        prior_msg_id: Option<String>,
        /// What to actually do, which differs by status.
        ///
        /// The sentence used to be baked in and ended "re-run with
        /// --allow-retry to override" — true of the resumable statuses and
        /// false of `confirmed` and `submitted`, which refuse the flag
        /// outright. One message serving two situations told half of its
        /// readers to try the one thing that cannot work for them.
        remedy: String,
    },

    /// The reservation was found rather than created and carries no AN tx
    /// hash. Nothing can tell from the record whether a burn is on the
    /// wire; `liveness` is the rendered verdict of what the lock could
    /// tell us. (This line named a field `another_run_is_live` for several
    /// rounds. There has never been one — and a bool is exactly the shape
    /// the verdict must not have.)
    #[error(
        "refuse: this withdrawal is already reserved ({prior_status}) and the record carries no \
         AN tx hash, so whether a burn is on the wire cannot be read from it — the hash is \
         written only after the send returns.\n\x20 {liveness}\n\x20 Record: {record_path}\n\x20 \
         --allow-retry does NOT override this, and re-running will not change it.\n\x20 1. \
         Reconcile on chain (advanced runbook, Case 3a): look for a sendTransaction from this \
         multisig to USDCBridge around the record's reserved_at.\n\x20 2. If a burn DID land, \
         write its hash into an_tx_hash and set status to \"burned\", then re-run with \
         --allow-retry — the run resumes at capture.\n\x20 3. Delete the record ONLY if step 1 \
         found no burn AND the line above said no other run holds this withdrawal. \"Could not be \
         determined\" is not that answer: it is what EVERY run gets on a filesystem without \
         flock, including one that is mid-send. Read this step by elimination — not the first \
         case, reconciliation clean — and you delete the record while another run is mid-send, \
         which is the second burn this refusal exists to prevent."
    )]
    ReservationInFlight {
        prior_status: String,
        prior_msg_id: Option<String>,
        record_path: String,
        /// Rendered sentence about whether another process holds the
        /// withdrawal lock. A `String` rather than a bool because the
        /// verdict has THREE values, not two: somebody holds it, nobody
        /// does, or it could not be determined — and the third is the one
        /// a bool would have to fold into one of the others. See
        /// [`crate::idempotency::liveness_verdict`].
        liveness: String,
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
            CliError::ArgInvalid {
                ..
            }
            | CliError::KeyFilePerms {
                ..
            }
            | CliError::Usage {
                ..
            }
            | CliError::Preflight {
                ..
            } => ExitCode::PreflightRefused,
            CliError::DuplicateInFlight {
                ..
            }
            | CliError::ReservationInFlight {
                ..
            } => ExitCode::DuplicateRefused,
            CliError::BurnOutcomeUnknown {
                ..
            } => ExitCode::BurnOutcomeUnknown,
            CliError::CaptureTimeout {
                ..
            } => ExitCode::CaptureTimeout,
            CliError::ProofFailed {
                ..
            } => ExitCode::ProofFailed,
            CliError::EthSubmitFailed {
                ..
            } => ExitCode::EthSubmitFailed,
        }
    }

    /// Which pipeline stage this error is associated with. Used by the
    /// `--json` error emitter.
    pub fn stage(&self) -> Stage {
        match self {
            CliError::ArgInvalid {
                ..
            }
            | CliError::KeyFilePerms {
                ..
            }
            | CliError::Usage {
                ..
            }
            | CliError::Preflight {
                ..
            }
            | CliError::DuplicateInFlight {
                ..
            }
            | CliError::ReservationInFlight {
                ..
            } => Stage::Preflight,
            CliError::BurnOutcomeUnknown {
                ..
            } => Stage::Burn,
            CliError::CaptureTimeout {
                ..
            } => Stage::Capture,
            CliError::ProofFailed {
                ..
            } => Stage::Prove,
            CliError::EthSubmitFailed {
                ..
            } => Stage::Submit,
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
                remedy: "…".into(),
            }
            .exit_code(),
            ExitCode::DuplicateRefused
        );
        assert_eq!(
            CliError::CaptureTimeout {
                an_tx: "0x…".into()
            }
            .exit_code(),
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
