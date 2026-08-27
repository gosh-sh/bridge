//! AN-side burn: compose + broadcast the single multisig transaction that
//! fires `USDCBridge.initiateWithdrawal(dstChainId, recipient)`.
//!
//! This module owns the only step in the whole CLI that spends money on
//! the AN side. It is the ONE place that reads `--from-keys` file
//! contents. Under no circumstance may key material appear in a log,
//! error, or JSON field.
//!
//! Encoding path (v1, in-library via tvm-sdk — no `tvm-cli` shell-out):
//!   1. `encode_message_body` against the USDCBridge ABI to produce the
//!      base64 payload cell for `initiateWithdrawal(dstChainId, recipient)`.
//!      `is_internal = true` because the payload will be carried by the
//!      multisig's outbound internal message, not signed externally.
//!   2. `process_message` against the multisig's `sendTransaction`, signed
//!      with the KeyPair loaded from `--from-keys`. The multisig forwards
//!      the payload with `value = 1 AN`, `cc[3] = <amount>` (ECC[3] USDC
//!      required by USDCBridge), and `flags = 1`.
//!
//! Bounce semantics: caller passes `bounce`. Ekaterina's spec defaults to
//! `bounce = true` so a bridge revert returns USDC to the multisig instead
//! of vanishing. The Python driver used `bounce = false` historically;
//! this Rust CLI defaults per spec (main.rs / args.rs) and honors whatever
//! caller passes here.
//!
//! Error mapping discipline:
//! - Key-file open/parse failures → `CliError::KeyFilePerms` / `Preflight`
//!   (exit 2). We have not broadcast anything.
//! - Body encoding failures → `Preflight` (exit 2). Not broadcast.
//! - `process_message` error → `BurnOutcomeUnknown` (exit 10). We can't
//!   reliably tell "not sent" from "sent, waiting" from the tvm-sdk error
//!   shape, so we default to the safe assumption that the operator must
//!   reconcile.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tvm_client::abi::{
    encode_message_body, Abi, CallSet, ParamsOfEncodeMessage, ParamsOfEncodeMessageBody, Signer,
};
use tvm_client::crypto::KeyPair;
use tvm_client::processing::{process_message, ParamsOfProcessMessage};
use tvm_client::ClientContext;

use crate::args::{FromAddress, ToAddress, UsdcAmount};
use crate::errors::{CliError, CliResult};
use crate::preflight::PreflightReport;

/// Vendored USDCBridge ABI — embedded at build time so the binary has no
/// runtime companion directory.
const USDC_BRIDGE_ABI_JSON: &str = include_str!("../abi/USDCBridge.abi.json");

/// Vendored multisig ABI (same file preflight uses for `getCustodians`).
const MULTISIG_ABI_JSON: &str = include_str!("../abi/UpdateCustodianMultisigWallet.abi.json");

/// ECC token id for USDC on Acki Nacki (`cc` map key on multisig
/// sendTransaction). Bare integer per the AN token model.
const USDC_ECC_ID: u32 = 3;

/// AN base-currency value attached to sendTransaction. Covers forwarding
/// fees for the two-hop bounce path (multisig → USDCBridge → …). Matches
/// the Python driver's 1e9 nano-token allotment.
const SEND_TX_VALUE_NANO: &str = "1000000000";

/// Multisig `flags` for internal-payment sendTransaction. `1` = "pay
/// forwarding fees from the message value" (i.e., value is not additive to
/// contract balance). This is what the current multisig ABI expects for a
/// standard forward.
const SEND_TX_FLAGS: u8 = 1;

/// Result of a successful burn broadcast. `an_tx_hash` is the multisig
/// transaction id returned by `process_message`. The
/// `WithdrawalInitiated` ExtOut id is captured downstream by the
/// orchestrator (via `bridge_relayer_daemon::withdraw_e2e::capture`).
#[derive(Debug, Clone)]
pub struct BurnReceipt {
    pub an_tx_hash: String,
    pub sent_amount_micro: u128,
    pub bounce: bool,
}

/// Fire the burn. Returns [`BurnReceipt`] on successful broadcast (as
/// attested by `process_message`); errors mid-broadcast surface as
/// [`CliError::BurnOutcomeUnknown`] (exit 10), NEVER as a preflight
/// refusal — once we've asked the SDK to send, "unknown" is the honest
/// state.
///
/// `context` is the shared tvm-sdk client (same one preflight built).
/// Passing it in avoids a second GraphQL handshake and keeps connection
/// state coherent.
pub async fn fire(
    context: &Arc<ClientContext>,
    preflight: &PreflightReport,
    from: &FromAddress,
    from_keys: &Path,
    to: &ToAddress,
    amount: &UsdcAmount,
    bounce: bool,
) -> CliResult<BurnReceipt> {
    // 1. Key file — load ONLY here, hold in a local, drop after send.
    let keys = load_keypair(from_keys)?;

    // 2. USDCBridge legacy `0:<hex>` address for the ABI `dest` field.
    //    Extended `dapp_id::account_id` is only understood by the routing
    //    layer; the ABI address type is workchain-legacy.
    let bridge_legacy = &preflight.usdc_bridge_legacy;

    // 3. Encode the initiateWithdrawal internal payload.
    let payload = encode_initiate_withdrawal_body(context, to, amount).await?;

    // 4. Compose multisig sendTransaction params.
    let params = json!({
        "dest":    bridge_legacy,
        "value":   SEND_TX_VALUE_NANO,
        "cc":      { USDC_ECC_ID.to_string(): amount.0.to_string() },
        "bounce":  bounce,
        "flags":   SEND_TX_FLAGS,
        "payload": payload,
    });

    // 5. Broadcast.
    let msig_abi = Abi::Json(MULTISIG_ABI_JSON.to_string());
    let encode = ParamsOfEncodeMessage {
        abi: msig_abi,
        address: Some(from.extended()),
        call_set: CallSet::some_with_function_and_input("sendTransaction", params),
        signer: Signer::Keys { keys },
        deploy_set: None,
        processing_try_index: None,
        signature_id: None,
    };
    let processed = process_message(
        context.clone(),
        ParamsOfProcessMessage {
            message_encode_params: encode,
            send_events: false,
            dapp_id: from.dapp_id_hex.clone(),
        },
        |_| async {},
    )
    .await
    .map_err(|e| CliError::BurnOutcomeUnknown {
        reason: format!(
            "sendTransaction on multisig {}: {} — reconcile via GraphQL before retrying",
            from.extended(),
            e.message(),
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;

    // 6. Classify the returned transaction. `process_message` can return
    //    Ok even when the compute phase reverted (exit_code != 0), so we
    //    must inspect the tx JSON before declaring success.
    let (aborted, exit_code) = classify_tx(&processed.transaction);
    if aborted || exit_code.is_some_and(|c| c != 0) {
        return Err(CliError::BurnOutcomeUnknown {
            reason: format!(
                "multisig sendTransaction aborted (exit_code={exit_code:?}, aborted={aborted}) — \
                 the multisig itself rejected the call before forwarding to USDCBridge; \
                 reconcile via GraphQL"
            ),
            source: None,
        });
    }

    let an_tx_hash = extract_tx_id(&processed.transaction)?;
    Ok(BurnReceipt {
        an_tx_hash,
        sent_amount_micro: amount.0,
        bounce,
    })
}

// -- Helpers -----------------------------------------------------------

/// Load a tvm-cli-style keys.json into a [`KeyPair`]. File format:
/// `{"public":"<64-hex>","secret":"<64-hex>"}`. Both halves must be
/// exactly 64 lowercase hex chars.
///
/// The returned `KeyPair` is the ONLY place the secret half lives; it is
/// consumed by `Signer::Keys` and dropped when `fire` returns.
fn load_keypair(path: &Path) -> CliResult<KeyPair> {
    let contents = std::fs::read_to_string(path).map_err(|_| CliError::KeyFilePerms {
        path: path.display().to_string(),
    })?;
    let json: Value = serde_json::from_str(&contents).map_err(|_| CliError::Preflight {
        reason: format!("--from-keys {}: not valid JSON", path.display()),
        source: None,
    })?;
    let public = json
        .get("public")
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| CliError::Preflight {
            reason: format!(
                "--from-keys {}: missing 'public' string field",
                path.display()
            ),
            source: None,
        })?;
    let secret = json
        .get("secret")
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| CliError::Preflight {
            reason: format!(
                "--from-keys {}: missing 'secret' string field",
                path.display()
            ),
            source: None,
        })?;
    Ok(KeyPair { public, secret })
}

/// Encode `USDCBridge.initiateWithdrawal(dstChainId, recipient)` as an
/// internal-message body cell. Returns the base64 body suitable for the
/// multisig sendTransaction `payload` field.
async fn encode_initiate_withdrawal_body(
    ctx: &Arc<ClientContext>,
    to: &ToAddress,
    _amount: &UsdcAmount,
) -> CliResult<String> {
    // The recipient is ABI type `bytes` — hex string of the raw 20-byte
    // EVM address (no `0x`, lowercase). Amount is NOT in this body: it
    // arrives via the ECC[3] cc[] map on the multisig sendTransaction.
    let params = json!({
        "dstChainId": to.chain_id.to_string(),
        "recipient":  hex::encode(to.address.as_slice()),
    });
    let abi = Abi::Json(USDC_BRIDGE_ABI_JSON.to_string());
    let call_set = CallSet::some_with_function_and_input("initiateWithdrawal", params)
        .expect("initiateWithdrawal params are always Some");
    let encoded = encode_message_body(
        ctx.clone(),
        ParamsOfEncodeMessageBody {
            abi,
            call_set,
            is_internal: true,
            signer: Signer::None,
            processing_try_index: None,
            address: None,
            signature_id: None,
        },
    )
    .await
    .map_err(|e| CliError::Preflight {
        reason: format!(
            "encode initiateWithdrawal body: {} — check USDCBridge ABI vs. args",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;
    Ok(encoded.body)
}

/// Extract `.id` (or `.hash`) from the tx JSON returned by
/// `process_message`. Normalizes to `0x`-prefixed 64-hex.
fn extract_tx_id(tx: &Value) -> CliResult<String> {
    let raw = tx
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| tx.get("hash").and_then(|v| v.as_str()))
        .ok_or_else(|| CliError::BurnOutcomeUnknown {
            reason: "process_message returned no transaction id — reconcile via GraphQL"
                .to_string(),
            source: None,
        })?;
    let bare = raw.trim_start_matches("0x");
    // Left-pad if the network returned a shorter id (shellnet quirk).
    let normalized = if bare.len() < 64 {
        format!("{:0>64}", bare)
    } else {
        bare.to_ascii_lowercase()
    };
    Ok(format!("0x{normalized}"))
}

/// `(aborted, compute.exit_code)` extraction. Mirrors
/// `acki-nacki-interface::classify_tx_json` but returns just the two
/// fields we need.
fn classify_tx(tx: &Value) -> (bool, Option<i32>) {
    let aborted = tx.get("aborted").and_then(|v| v.as_bool()).unwrap_or(false);
    let exit_code = tx
        .get("compute")
        .and_then(|c| c.get("exit_code"))
        .and_then(|v| v.as_i64())
        .or_else(|| tx.get("exit_code").and_then(|v| v.as_i64()))
        .map(|c| c as i32);
    (aborted, exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tx_id_prefers_id_field() {
        let tx = json!({ "id": "0xabc", "hash": "0xdef" });
        assert_eq!(
            extract_tx_id(&tx).unwrap(),
            format!("0x{:0>64}", "abc"),
        );
    }

    #[test]
    fn extract_tx_id_falls_back_to_hash() {
        let tx = json!({ "hash": "abc" });
        assert_eq!(
            extract_tx_id(&tx).unwrap(),
            format!("0x{:0>64}", "abc"),
        );
    }

    #[test]
    fn extract_tx_id_full_64_hex_lowercased() {
        let full = "A".repeat(64);
        let tx = json!({ "id": format!("0x{full}") });
        assert_eq!(
            extract_tx_id(&tx).unwrap(),
            format!("0x{}", "a".repeat(64)),
        );
    }

    #[test]
    fn extract_tx_id_missing_is_burn_unknown() {
        let tx = json!({});
        match extract_tx_id(&tx) {
            Err(CliError::BurnOutcomeUnknown { .. }) => (),
            other => panic!("expected BurnOutcomeUnknown, got {other:?}"),
        }
    }

    #[test]
    fn classify_tx_success() {
        let tx = json!({ "aborted": false, "compute": { "exit_code": 0 } });
        assert_eq!(classify_tx(&tx), (false, Some(0)));
    }

    #[test]
    fn classify_tx_aborted() {
        let tx = json!({ "aborted": true, "compute": { "exit_code": 108 } });
        assert_eq!(classify_tx(&tx), (true, Some(108)));
    }

    #[test]
    fn classify_tx_missing_fields_defaults_to_ok() {
        let tx = json!({});
        assert_eq!(classify_tx(&tx), (false, None));
    }

    #[test]
    fn load_keypair_reads_public_and_secret() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"public":"{p}","secret":"{s}"}}"#,
            p = "0".repeat(64),
            s = "1".repeat(64),
        )
        .unwrap();
        let kp = load_keypair(&path).unwrap();
        assert_eq!(kp.public, "0".repeat(64));
        assert_eq!(kp.secret, "1".repeat(64));
    }

    #[test]
    fn load_keypair_missing_public_is_preflight() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"secret":"{}"}}"#, "1".repeat(64)).unwrap();
        match load_keypair(&path) {
            Err(CliError::Preflight { reason, .. }) => {
                assert!(reason.contains("missing 'public'"), "got {reason}");
            }
            other => panic!("expected Preflight, got {other:?}"),
        }
    }

    #[test]
    fn load_keypair_missing_file_is_keyperms() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nope.json");
        match load_keypair(&path) {
            Err(CliError::KeyFilePerms { .. }) => (),
            other => panic!("expected KeyFilePerms, got {other:?}"),
        }
    }
}
