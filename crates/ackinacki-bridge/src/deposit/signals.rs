//! SIGINT, SIGTERM and SIGHUP end a deposit run cleanly: the run's future
//! is dropped, which kills the prover subprocess and releases every lock,
//! and the operation stays where its record says, ready for `--resume`.
//! SIGKILL cannot be caught; for it, the prover holds its own lock (prover.rs).
//!
//! The handlers stay installed for the rest of the process once
//! [`until_signal`] has run: the run is meant to be the last thing the
//! process does.

use std::{future::Future, path::Path};

use tokio::signal::unix::{signal, Signal as Stream, SignalKind};

use crate::{
    deposit::store::{OpStage, Store},
    errors::{CliError, ExitCode, Stage},
};

/// A signal that ends a deposit run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGINT: Ctrl-C.
    Int,
    /// SIGTERM: `kill <pid>`, a service manager stopping the run.
    Term,
    /// SIGHUP: the terminal went away.
    Hup,
}

impl Signal {
    /// The signal's name as `kill -l` spells it, with the `SIG` prefix.
    pub fn name(self) -> &'static str {
        match self {
            Signal::Int => "SIGINT",
            Signal::Term => "SIGTERM",
            Signal::Hup => "SIGHUP",
        }
    }
}

/// Resolves when `s` delivers a signal; never, when no handler could be
/// installed (the signal's default action stays in force) or the stream
/// has closed (the runtime is shutting down and nothing arrives any more).
async fn recv(s: &mut Option<Stream>) {
    if let Some(s) = s {
        if s.recv().await.is_some() {
            return;
        }
    }
    std::future::pending::<()>().await
}

/// Runs `fut` to completion, or until SIGINT, SIGTERM or SIGHUP arrives.
/// On a signal `fut` is dropped before this returns, so every subprocess
/// it spawned with `kill_on_drop` is killed and every lock it held is
/// released.
///
/// The handlers are installed before `fut` is first polled. A signal and
/// the end of `fut` noticed in the same wakeup count as the signal: Ctrl-C
/// reaches the prover too, and its death is not the run's own failure.
pub async fn until_signal<F: Future>(fut: F) -> Result<F::Output, Signal> {
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut term = signal(SignalKind::terminate()).ok();
    let mut hup = signal(SignalKind::hangup()).ok();
    tokio::select! {
        biased;
        () = recv(&mut int) => Err(Signal::Int),
        () = recv(&mut term) => Err(Signal::Term),
        () = recv(&mut hup) => Err(Signal::Hup),
        out = fut => Ok(out),
    }
}

/// The exit code of a run that stops with its operation at `stage` (the
/// last one on disk), or with no operation at all: before the wallet was
/// asked 2, while the EVM outcome is open 30, waiting for the Acki Nacki
/// side 31, proving 32, from the proof on 34.
pub fn exit_for(stage: Option<OpStage>) -> ExitCode {
    match stage {
        None | Some(OpStage::Reserved) => ExitCode::PreflightRefused,
        Some(OpStage::Requested | OpStage::Signed | OpStage::Abandoned) => {
            ExitCode::DepositOutcomeUnknown
        },
        Some(OpStage::Confirmed) => ExitCode::AnWaitTimeout,
        Some(OpStage::Anchored) => ExitCode::DepositProofFailed,
        Some(OpStage::Proved | OpStage::Finalizing | OpStage::Credited | OpStage::Failed) => {
            ExitCode::CreditUnconfirmed
        },
    }
}

/// The error a run stopped by `sig` ends with. `op` is the operation the
/// run was driving, if it had created or resumed one by then; its exit
/// code follows the stage its record in `state_dir` says.
///
/// A record that cannot be read leaves the stage unknown: that is exit 34,
/// the code for an outcome that is not known, and never "nothing was sent".
pub fn interrupted(sig: Signal, state_dir: &Path, op: Option<String>) -> CliError {
    let nothing_sent = || CliError::Preflight {
        reason: format!(
            "interrupted by {} before the wallet was asked (nothing was sent)",
            sig.name()
        ),
        source: None,
    };
    let Some(op) = op else {
        return nothing_sent();
    };
    let stage = match Store::open(state_dir).and_then(|s| s.load(&op)) {
        Ok(rec) => rec.stage,
        Err(e) => {
            let why = match e {
                CliError::Preflight {
                    reason, ..
                } => reason,
                other => other.to_string(),
            };
            return CliError::deposit(
                ExitCode::CreditUnconfirmed,
                Stage::Deposit,
                Some(&op),
                format!(
                    "interrupted by {}; the record could not be read ({why}), so how far the \
                     operation got is unknown; continue with --resume {op}",
                    sig.name()
                ),
            );
        },
    };
    match exit_for(Some(stage)) {
        ExitCode::PreflightRefused => nothing_sent(),
        exit => CliError::deposit(
            exit,
            Stage::Deposit,
            Some(&op),
            format!(
                "interrupted by {} at stage {stage:?}; the operation is recorded, continue with \
                 --resume {op}",
                sig.name()
            ),
        ),
    }
}

/// Operation records at a chosen stage, for this module's tests and for
/// the prover's process tests.
#[cfg(test)]
pub mod tests_support {
    use std::path::Path;

    use alloy_primitives::{Address, Bytes, B256, U256};

    use crate::deposit::store::{
        CreditInfo, DepositInfo, FailReason, Failure, FinalizeInfo, OpParams, OpRecord, OpStage,
        RequestInfo, Store, TxClaim,
    };

    /// Writes an operation that has reached `stage` into the store at
    /// `state_dir`, with the fields that stage cannot exist without, and
    /// returns its id.
    pub fn record_at(state_dir: &Path, stage: OpStage) -> String {
        let store = Store::open(state_dir).unwrap();
        let mut rec = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 11_155_111,
                bridge: Address::repeat_byte(0xb1),
                to: format!("{}::{}", "00".repeat(32), "a3".repeat(32)),
                amount_units: 12_500_000,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            "bd44".into(),
        );
        rec.stage = stage;
        rec.from = Some(Address::repeat_byte(0x11));
        rec.request = Some(RequestInfo {
            nonce_before: 7,
            from_block: 100,
            calldata: Bytes::from_static(&[0xa4, 0x1d, 0x02, 0x29]),
            wallet_hash: None,
        });
        rec.tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(2),
            tx_nonce: 7,
        });
        rec.deposit = Some(DepositInfo {
            deposit_id: U256::from(5),
            block_number: 101,
            block_hash: B256::repeat_byte(3),
            block_log_index: 0,
            receipt_log_index: 1,
            access_list_rlp_len: 1,
            voucher_account: "ee".repeat(32),
        });
        rec.finalize = Some(FinalizeInfo {
            voucher_code_hash: "bd44".into(),
            voucher_account: "ee".repeat(32),
            sends: 1,
        });
        rec.credit = Some(CreditInfo {
            confirm_tx: "c1".repeat(32),
            delivery_tx: "d1".repeat(32),
            via_events: false,
        });
        if stage == OpStage::Failed {
            rec.failure = Some(Failure {
                at: OpStage::Anchored,
                reason: FailReason::Unprovable,
                detail: "the transaction's shape cannot be proven".into(),
                exit: 35,
            });
        }
        store.write(&mut rec).unwrap();
        rec.op_id
    }
}

// No test here installs a signal handler: tokio keeps one for the life of
// the process, and a test binary that swallows SIGINT and SIGTERM outlives
// the `timeout` and the Ctrl-C meant to stop it. `until_signal` runs, and
// receives real signals, in the child processes of the prover's tests.
#[cfg(test)]
mod tests {
    use super::{tests_support::record_at, *};

    /// Every stage an operation can be in.
    const STAGES: [OpStage; 10] = [
        OpStage::Reserved,
        OpStage::Requested,
        OpStage::Signed,
        OpStage::Confirmed,
        OpStage::Anchored,
        OpStage::Proved,
        OpStage::Finalizing,
        OpStage::Credited,
        OpStage::Failed,
        OpStage::Abandoned,
    ];

    #[test]
    fn the_exit_code_follows_the_last_recorded_stage() {
        let table = [
            (None, 2),
            (Some(OpStage::Reserved), 2),
            (Some(OpStage::Requested), 30),
            (Some(OpStage::Signed), 30),
            (Some(OpStage::Abandoned), 30),
            (Some(OpStage::Confirmed), 31),
            (Some(OpStage::Anchored), 32),
            (Some(OpStage::Proved), 34),
            (Some(OpStage::Finalizing), 34),
            (Some(OpStage::Credited), 34),
            (Some(OpStage::Failed), 34),
        ];
        for (stage, exit) in table {
            assert_eq!(exit_for(stage).as_i32(), exit, "{stage:?}");
        }
        assert_eq!(
            table.len(),
            STAGES.len() + 1,
            "a row per stage and one for none"
        );
    }

    #[test]
    fn signals_are_named_as_the_shell_names_them() {
        assert_eq!(Signal::Int.name(), "SIGINT");
        assert_eq!(Signal::Term.name(), "SIGTERM");
        assert_eq!(Signal::Hup.name(), "SIGHUP");
    }

    #[test]
    fn an_interruption_before_any_operation_sent_nothing() {
        let d = tempfile::tempdir().unwrap();
        let e = interrupted(Signal::Int, d.path(), None);
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
        let m = e.to_string();
        assert!(m.contains("SIGINT"), "{m}");
        assert!(m.contains("nothing was sent"), "{m}");
    }

    #[test]
    fn an_interruption_answers_with_the_recorded_stage_and_how_to_resume() {
        for stage in STAGES {
            let d = tempfile::tempdir().unwrap();
            let op = record_at(d.path(), stage);
            let e = interrupted(Signal::Term, d.path(), Some(op.clone()));
            assert_eq!(e.exit_code(), exit_for(Some(stage)), "{stage:?}");
            let m = e.to_string();
            assert!(m.contains("SIGTERM"), "{m}");
            if stage == OpStage::Reserved {
                assert!(m.contains("nothing was sent"), "{m}");
                continue;
            }
            assert!(!m.contains("nothing was sent"), "{m}");
            assert!(m.contains(&format!("at stage {stage:?};")), "{m}");
            assert!(m.contains(&format!("--resume {op}")), "{m}");
            assert_eq!(e.op_id(), Some(op.as_str()));
        }
    }

    #[test]
    fn an_operation_whose_record_cannot_be_read_is_not_said_to_have_sent_nothing() {
        let d = tempfile::tempdir().unwrap();
        let op = crate::deposit::store::Store::new_op_id(); // never written
        let e = interrupted(Signal::Hup, d.path(), Some(op.clone()));
        assert_eq!(e.exit_code(), ExitCode::CreditUnconfirmed);
        let m = e.to_string();
        assert!(m.contains("SIGHUP"), "{m}");
        assert!(!m.contains("nothing was sent"), "{m}");
        assert!(m.contains("could not be read (deposit operation"), "{m}");
        assert!(m.contains(&format!("--resume {op}")), "{m}");
    }
}
