//! How the Acki Nacki bridge refuses `finalizeDeposit`, and what the CLI
//! does about each refusal.
//!
//! Codes raised before `tvm.accept()` reject the external message: there
//! is no transaction, and the code arrives in the send error. Codes
//! raised after it are a transaction with that exit code.

/// The deposit amount is zero.
pub const ERR_ZERO_AMOUNT: i32 = 204;
/// The amount does not fit the bridge's accounting.
pub const ERR_OVERFLOW: i32 = 214;
/// The bridge rejected the proof.
pub const ERR_INVALID_ZKPROOF: i32 = 220;
/// The source chain or EVM bridge is not trusted by the bridge.
pub const ERR_UNSUPPORTED_SRC_CHAIN: i32 = 222;
/// The bridge does not know the anchored block yet.
pub const ERR_UNKNOWN_BLOCK: i32 = 224;
/// The recipient is the zero address.
pub const ERR_ZERO_RECIPIENT: i32 = 230;
/// Deposits are paused.
pub const ERR_PAUSED: i32 = 231;

/// The outcome of sending `finalizeDeposit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizeSend {
    /// The node refused the external message; no transaction exists.
    Rejected {
        /// The exit code from the structured error data, if any.
        exit_code: Option<i32>,
        /// The error text.
        message: String,
    },
    /// The message ran as a transaction.
    Executed {
        /// The transaction id.
        tx_id: String,
        /// The compute or action phase aborted.
        aborted: bool,
        /// The exit code of the transaction, if known.
        exit_code: Option<i32>,
    },
    /// The send may or may not have reached the network.
    Unknown {
        /// The error text.
        message: String,
    },
    /// A local failure before anything left the process (for example an
    /// encoding error): the send is not in doubt.
    NotSent {
        /// The error text.
        message: String,
    },
}

/// What the driver does after a send outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaction {
    /// Deposits are paused: wait and send again.
    WaitPaused,
    /// The anchored block is not known yet: wait for the next anchor.
    BackToAnchor,
    /// Final refusal.
    Refused {
        /// The bridge exit code.
        code: i32,
        /// What the operator should do about it.
        hint: &'static str,
    },
    /// The message was accepted: confirm the credit.
    ToCredit,
    /// Nothing final is known: send again.
    Retry,
    /// Nothing left the process; terminal, never retried and never
    /// treated as a send in doubt.
    NotSent {
        /// The error text.
        message: String,
    },
}

/// The reaction to a bridge exit code.
fn by_code(code: i32) -> Reaction {
    match code {
        ERR_PAUSED => Reaction::WaitPaused,
        ERR_UNKNOWN_BLOCK => Reaction::BackToAnchor,
        ERR_UNSUPPORTED_SRC_CHAIN => Reaction::Refused {
            code,
            hint: "the bridge no longer trusts this chain and EVM bridge (the allowlist was \
                   changed or wiped by a code upgrade); resume with --resume once its owner \
                   restores it",
        },
        ERR_INVALID_ZKPROOF => Reaction::Refused {
            code,
            hint: "the bridge rejected the proof: the installed prover does not match the \
                   bridge's verification key; keep the work directory and report it",
        },
        ERR_OVERFLOW | ERR_ZERO_AMOUNT | ERR_ZERO_RECIPIENT => Reaction::Refused {
            code,
            hint: "unreachable after the CLI's own checks: report it as a bug with the work \
                   directory",
        },
        _ => Reaction::Refused {
            code,
            hint: "unexpected bridge exit code; report it with the work directory",
        },
    }
}

/// The reaction to a send outcome.
pub fn react(s: &FinalizeSend) -> Reaction {
    match s {
        FinalizeSend::Rejected {
            exit_code: Some(c),
            ..
        } => by_code(*c),
        FinalizeSend::Rejected {
            exit_code: None, ..
        }
        | FinalizeSend::Unknown {
            ..
        } => Reaction::Retry,
        FinalizeSend::NotSent {
            message,
        } => Reaction::NotSent {
            message: message.clone(),
        },
        FinalizeSend::Executed {
            aborted: false,
            exit_code: Some(0) | None,
            ..
        } => Reaction::ToCredit,
        FinalizeSend::Executed {
            exit_code: Some(c),
            ..
        } if *c != 0 => by_code(*c),
        FinalizeSend::Executed {
            ..
        } => Reaction::Retry,
    }
}

/// The exit code a `tvm_client` error carries in its data. Only the
/// structured fields: a code quoted inside a message is not trusted.
pub fn exit_code_from_sdk(data: &serde_json::Value) -> Option<i32> {
    data.get("exit_code")
        .or_else(|| data.pointer("/local_error/data/exit_code"))
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rejected(code: i32) -> FinalizeSend {
        FinalizeSend::Rejected {
            exit_code: Some(code),
            message: String::new(),
        }
    }

    fn executed(code: i32) -> FinalizeSend {
        FinalizeSend::Executed {
            tx_id: "ab".into(),
            aborted: true,
            exit_code: Some(code),
        }
    }

    #[test]
    fn the_table_of_refusals() {
        assert_eq!(react(&rejected(231)), Reaction::WaitPaused);
        for c in [214, 204, 230] {
            assert!(matches!(react(&rejected(c)), Reaction::Refused { code, .. } if code == c));
        }
        assert!(matches!(react(&rejected(222)), Reaction::Refused {
            code: 222,
            ..
        }));
        assert!(matches!(react(&executed(220)), Reaction::Refused {
            code: 220,
            ..
        }));
        assert_eq!(react(&executed(224)), Reaction::BackToAnchor);
        assert_eq!(
            react(&FinalizeSend::Executed {
                tx_id: "ab".into(),
                aborted: false,
                exit_code: Some(0)
            }),
            Reaction::ToCredit
        );
    }

    #[test]
    fn nothing_without_a_code_is_final() {
        assert_eq!(
            react(&FinalizeSend::Rejected {
                exit_code: None,
                message: "timeout".into()
            }),
            Reaction::Retry
        );
        assert_eq!(
            react(&FinalizeSend::Unknown {
                message: "reset".into()
            }),
            Reaction::Retry
        );
    }

    #[test]
    fn a_local_failure_before_sending_is_terminal_not_retried() {
        let r = react(&FinalizeSend::NotSent {
            message: "encode failed".into(),
        });
        assert_eq!(r, Reaction::NotSent {
            message: "encode failed".into()
        });
        assert_ne!(r, Reaction::Retry);
    }

    #[test]
    fn the_code_is_read_from_the_error_data_not_its_text() {
        assert_eq!(exit_code_from_sdk(&json!({ "exit_code": 231 })), Some(231));
        assert_eq!(
            exit_code_from_sdk(&json!({ "local_error": { "data": { "exit_code": 222 } } })),
            Some(222)
        );
        assert_eq!(
            exit_code_from_sdk(&json!({ "message": "exit_code=220" })),
            None
        );
    }

    #[test]
    fn hints_never_name_setters_the_bridge_does_not_have() {
        for c in [214, 204, 220, 222, 230] {
            if let Reaction::Refused {
                hint, ..
            } = react(&rejected(c))
            {
                for ghost in ["setExpectedBridge", "attestBlockHash", "setMintCap"] {
                    assert!(!hint.contains(ghost), "{c}: {hint}");
                }
            }
        }
    }
}
