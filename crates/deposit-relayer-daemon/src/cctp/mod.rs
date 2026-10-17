//! CCTP forwarder: a burn on a source chain becomes a hub `Deposit`.
//!
//! Circle attests the burn off-chain and does not call the mint recipient.
//! This module polls Iris for that attestation and sends two hub transactions:
//! `CctpHubReceiver.receiveBurn` (mint and park) and `forward` (the
//! `bridge.deposit` call). The existing deposit relayer then proves the
//! `Deposit` of the `forward` transaction as it would any other one.
//!
//! Finality is Iris's `status`, not a block-depth on the source chain. A
//! `pending_confirmations` response is retried; only `complete` with a real
//! attestation is submitted.

mod forward;
mod iris;

pub use forward::{
    run_cctp_daemon, run_cctp_forward, CctpDaemonConfig, CctpForwardConfig, ForwardState,
    MAX_FORWARD_FAILURES,
};
pub use iris::{classify_messages, AttestedMessage, IrisClass, IrisClient};
