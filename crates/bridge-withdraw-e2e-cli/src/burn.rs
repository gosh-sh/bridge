//! AN-side burn: compose + broadcast the single multisig transaction that
//! fires `USDCBridge.initiateWithdrawal(dstChainId, recipient)`.
//!
//! This module owns the only step in the whole CLI that spends money on
//! the AN side. It is the ONE place that reads `--from-keys` file
//! contents. Under no circumstance may key material appear in a log,
//! error, or JSON field.
//!
//! Encoding path (v1): shell out to `tvm-cli`. Mirrors the Python
//! driver's `bridge_e2e.py::call_initiate_withdrawal` (lines 213–238):
//!   1. `tvm-cli -j body --abi <USDCBridge.abi.json> initiateWithdrawal
//!      '{"dstChainId":"<n>","recipient":"<hex>"}'` → base64 Message cell.
//!   2. `tvm-cli -j callx --abi <UpdateCustodianMultisigWallet.abi.json>
//!      --addr <from> --keys <from-keys> -m sendTransaction
//!      '{"dest":"0:<usdc_bridge_acc>","value":"1000000000",
//!        "cc":{"3":"<amount_micro>"},"bounce":true,"flags":1,
//!        "payload":"<BODY>"}'`.
//! `bounce: true` by default per Ekaterina's decision — if the bridge
//! reverts, USDC returns to the multisig instead of vanishing. Opt out
//! only via `--unsafe-no-bounce` (not yet wired).

use crate::args::{FromAddress, ToAddress, UsdcAmount};
use crate::errors::CliResult;
use crate::preflight::PreflightReport;

/// Result of a successful burn broadcast. `an_tx_hash` is the multisig's
/// external message id (tvm-cli JSON `.message_id`). The
/// WithdrawalInitiated ExtOut id is captured later by the orchestrator.
#[derive(Debug, Clone)]
pub struct BurnReceipt {
    pub an_tx_hash: String,
    pub sent_amount_micro: u128,
    pub bounce: bool,
}

/// Fire the burn. Returns [`BurnReceipt`] on successful broadcast (as far
/// as tvm-cli can attest); errors mid-broadcast surface as
/// [`crate::errors::CliError::BurnOutcomeUnknown`] (exit 10), NEVER as
/// a preflight refusal — once we've started sending, "unknown" is the
/// honest state.
pub async fn fire(
    _preflight: &PreflightReport,
    _from: &FromAddress,
    _from_keys: &std::path::Path,
    _to: &ToAddress,
    _amount: &UsdcAmount,
    _bounce: bool,
) -> CliResult<BurnReceipt> {
    unimplemented!("burn::fire — implement in third commit")
}
