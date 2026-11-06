//! Relayer for the **EVM→Acki Nacki deposit direction**.
//!
//! This is the mirror of `bridge-relayer-daemon` (which handles AN→ETH state
//! via `verifyBlock`). Here the relayer automates the *deposit* leg of the
//! bridge:
//!
//! 1. **Listen** for `AckiNackiBridge.Deposit(depositId, sender, amount,
//!    anAccount, timestamp, token)` events on Ethereum, surfacing only deposits
//!    under a configurable confirmation depth ([`source::EthLogSource`]).
//! 2. **Collect the receipt proof** — the deposit block's header and the
//!    Merkle-Patricia proof of the deposit transaction's receipt against the
//!    header's `receiptsRoot` ([`witness::ReceiptWitnessBuilder`], built on
//!    [`deposit_receipt::fetch`]). This takes a few RPC calls; nothing is
//!    proven off chain.
//! 3. **Submit** `eccBridge.finalizeDeposit(chainId, header, txIndex,
//!    receiptProof, logIndex)` on Acki Nacki
//!    ([`submitter::AnInterfaceSubmitter`]). The bridge requires the header's
//!    hash to be an accepted block of `chainId`, walks the receipt proof, reads
//!    the `Deposit` log of that receipt and deploys a per-deposit
//!    `DepositVoucher`, which mints.
//!
//! ## Architecture
//!
//! Three trait seams keep the loop testable:
//!
//! ```text
//! ┌────────────────────────┐   ┌──────────────────────────┐   ┌────────────────────────┐
//! │ DepositSource          │──▶│ Relayer::tick()          │──▶│ AnSubmitter            │
//! │  - EthLogSource (alloy)│   │ 1. fetch deposit event   │   │  - AnInterfaceSubmitter│
//! │  - InMemoryDepositSrc  │   │ 2. voucher pre-check     │   │  - MockAnSubmitter     │
//! └────────────────────────┘   │ 3. block accepted on AN? │   └────────────────────────┘
//!                              │ 4. build receipt proof   │
//!                              │ 5. submit + persist      │
//! ┌────────────────────────┐   └────────────┬─────────────┘
//! │ WitnessBuilder         │◀───────────────┘
//! │  - ReceiptWitnessBuild │   ┌──────────────────────────┐
//! │  - MockWitnessBuilder  │   │ state.json (depositId)   │
//! └────────────────────────┘   └──────────────────────────┘
//! ```
//!
//! A second `finalizeDeposit` for the same deposit fails in the voucher
//! constructor (exit 51), so re-submission is safe.

pub mod an_config;
pub mod daemon;
pub mod dirs;
pub mod error;
#[cfg(test)]
mod fake_rpc;
pub mod metrics;
pub mod redact;
pub mod relayer;
pub mod source;
pub mod state;
pub mod submitter;
pub mod supported_chains;
pub mod types;
pub mod voucher;
pub mod witness;

pub use an_config::{bridge_abi, AnConfig, BRIDGE_ABI_JSON};
pub use daemon::{
    BackoffConfig, DaemonRunSummary, LastOutcome, RelayerMetrics, RelayerMetricsSnapshot,
};
pub use dirs::{
    prepare_dir, refuse_unrecognised_state, ServiceDirs, LOG_FILE_PREFIX, STATE_FILE_NAME,
};
pub use error::RelayerError;
pub use relayer::{Relayer, RelayerConfig, TickOutcome};
pub use source::{
    fetch_deposit_from_receipt, is_retryable_eth_rpc_error, receipt_log_index_from_block_log,
    resolve_from_block, AckiNackiBridge, DepositSource, EthLogSource, InMemoryDepositSource,
    BRIDGE_DEPLOY_BLOCK_ENV,
};
pub use state::{DeploymentIdentity, DepositLogRef, OutstandingDeposit, RelayerState, StateLock};
pub use submitter::{
    AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, MockAnSubmitter, SubmitOutcome,
};
pub use supported_chains::{
    is_supported_deposit_chain, supported_deposit_chain_name, supported_deposit_chains_display,
    CHAIN_ID_SEPOLIA, SUPPORTED_DEPOSIT_CHAIN_IDS,
};
pub use types::DepositEvent;
pub use witness::{
    check_payload_len, check_witness_matches, DepositWitness, MockWitnessBuilder,
    ReceiptWitnessBuilder, WitnessBuilder, MAX_FINALIZE_PAYLOAD_BYTES,
};
