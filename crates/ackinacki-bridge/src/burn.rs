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
//! Error mapping discipline. The split into [`compose`] and [`send`] is
//! exactly this discipline made structural:
//! - [`compose`] is the **pre-send** half. Key-file open/parse failures →
//!   `CliError::KeyFilePerms` / `Preflight`; body encoding, the multisig
//!   call composition, and the validating `encode_message` → `Preflight`.
//!   Every one of them is exit 2 and provably means nothing was broadcast,
//!   which is what lets the orchestrator take the idempotency reserve
//!   *after* this half and before the next one.
//! - [`send`] is the **post-commit** half. Every failure is
//!   `BurnOutcomeUnknown` (exit 10), including a clean-looking `Ok` whose
//!   transaction aborted: we can't reliably tell "not sent" from "sent,
//!   waiting" from the tvm-sdk error shape, so we default to the safe
//!   assumption that the operator must reconcile.
//! - Neither half lets an SDK error text through. The signing path
//!   interpolates the public key verbatim and the secret's first eight
//!   characters into its messages (`tvm_client/src/crypto/errors.rs:118-126`),
//!   so the error CODE is kept as a classifier and the text is dropped.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::debug;
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

/// Everything needed to broadcast, already built and proven encodable.
///
/// `Debug` is implemented by hand, never derived. `ParamsOfEncodeMessage`
/// *does* derive `Debug` and holds `Signer::Keys { keys }`, so a derive
/// would put the owner's public key into every `{:?}` — including the one
/// `expect_err` prints from a failing test. Tests need some `Debug` to call
/// `expect_err` at all; this gives them one that reveals nothing.
///
/// Holds the owner `KeyPair` for as long as the caller holds this value —
/// between `compose` and `send`, which is a handful of microseconds plus
/// one idempotency file write. Same discipline as before: it exists only
/// inside these two calls, never reaches a log, an error or a JSON field,
/// and is dropped when `send` returns.
pub struct ComposedBurn {
    encode: ParamsOfEncodeMessage,
    bounce: bool,
    amount_micro: u128,
    /// Id of the message produced by the validating encode in [`compose`].
    ///
    /// **Not** the id of the message [`send`] broadcasts. The ABI header is
    /// `["pubkey", "time", "expire"]`, so `process_message` re-encodes with
    /// a fresh `time`/`expire` per try and gets a different id. This value
    /// exists to prove the encode ran, and for logs — never persist it as
    /// "the transaction we sent". Making an id that survives to the wire is
    /// the `send_message`-based redesign in Part 5.
    validated_message_id: String,
}

impl std::fmt::Debug for ComposedBurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposedBurn")
            .field("bounce", &self.bounce)
            .field("amount_micro", &self.amount_micro)
            .field("validated_message_id", &self.validated_message_id)
            .finish_non_exhaustive()
    }
}

/// Build the multisig `sendTransaction` without touching the network, and
/// prove it encodes.
///
/// Every fallible pre-send step lives here: reading `--from-keys`, encoding
/// the `initiateWithdrawal` payload, composing the multisig call, **and
/// running `encode_message` itself**. Failures are `KeyFilePerms` /
/// `Preflight` (exit 2) and provably mean nothing was broadcast — that is
/// the property that lets the orchestrator place the idempotency reserve
/// *after* this and before [`send`].
///
/// The encode looks redundant and is not. `process_message` re-encodes
/// internally before sending
/// (`tvm_client/src/processing/process_message.rs:45`), so composing only
/// the *params* would leave ABI encoding and signing to fail AFTER the
/// reserve, on a withdrawal that never reached the wire — the exact bug
/// this split exists to close. The throwaway encode moves those failures
/// in front of the reserve. It is local and cheap; do not "optimise" it
/// away.
///
/// It does not make `send`'s re-encode infallible — the inputs are
/// identical and the encoder is deterministic, but that is an observation
/// about the SDK, not a promise from it.
///
/// Do not move network work in here, and do not let `send` grow a fallible
/// local step that could have run here: the reserve sits between them
/// precisely because this side cannot reach the wire.
pub async fn compose(
    context: &Arc<ClientContext>,
    preflight: &PreflightReport,
    from: &FromAddress,
    from_keys: &Path,
    to: &ToAddress,
    amount: &UsdcAmount,
    bounce: bool,
) -> CliResult<ComposedBurn> {
    let keys = load_keypair(from_keys)?;

    // The keys we are about to sign with must still be the keys preflight
    // approved. `compose` re-reads `--from-keys`, so between the preflight
    // check and this line the file can have changed — a swap, a partial
    // rewrite, an operator editing it in another terminal. Catch that here,
    // with our own message, rather than letting `encode_message` catch it
    // with the SDK's (see below for why that matters).
    //
    // **Immediately after the load, and before anything takes `keys`.** An
    // earlier draft put this after the `ParamsOfEncodeMessage` literal,
    // where `Signer::Keys { keys }` has already moved the value — E0382,
    // use of a moved value. `KeyPair` is `Clone` but not `Copy`
    // (`tvm_client/src/crypto/keys.rs:42`), so the compiler's suggestion
    // there would be to clone, and cloning is the wrong fix: it makes a
    // second heap copy of the secret for no reason. The check needs nothing
    // that has not already happened at this point, so it simply belongs
    // here.
    //
    // `preflight.owner_pubkey_hex` is the on-chain custodian pubkey that
    // check 5 already matched this file against; `pubkeys_equal` compares
    // the two as VALUES — normalising `0x`, case and padding — so reuse it
    // rather than `!=` on raw strings. `load_keypair` above normalises the
    // same way, so both sides of this comparison and the bytes handed to
    // the SDK all agree; while they did not, a `0x`-prefixed keys.json
    // dead-ended here on every attempt. Neither key is echoed — the
    // on-chain half would be safe to print, but there is nothing to
    // disambiguate here that the message does not say.
    if !crate::preflight::pubkeys_equal(&keys.public, &preflight.owner_pubkey_hex) {
        return Err(CliError::Preflight {
            reason: format!(
                "--from-keys changed between preflight and signing: the public key in {} is no \
                 longer the one verified against the multisig owner of {}. Nothing was sent. \
                 Re-run.",
                from_keys.display(),
                from.extended(),
            ),
            source: None,
        });
    }

    // USDCBridge legacy `0:<hex>` address for the ABI `dest` field.
    // Extended `dapp_id::account_id` is only understood by the routing
    // layer; the ABI address type is workchain-legacy.
    let bridge_legacy = &preflight.usdc_bridge_legacy;
    let payload = encode_initiate_withdrawal_body(context, to, amount).await?;

    let params = json!({
        "dest":    bridge_legacy,
        "value":   SEND_TX_VALUE_NANO,
        "cc":      { USDC_ECC_ID.to_string(): amount.0.to_string() },
        "bounce":  bounce,
        "flags":   SEND_TX_FLAGS,
        "payload": payload,
    });

    let encode = ParamsOfEncodeMessage {
        abi: Abi::Json(MULTISIG_ABI_JSON.to_string()),
        // tvm-sdk `encode_message` wants legacy `0:<acc>` here; the extended
        // `dapp::acc` form is only understood by the tvm-cli `--addr` flag.
        // Routing dapp is passed separately via `ParamsOfProcessMessage.dapp_id`
        // in `send`.
        address: Some(from.legacy()),
        call_set: CallSet::some_with_function_and_input("sendTransaction", params),
        signer: Signer::Keys {
            keys,
        },
        deploy_set: None,
        processing_try_index: None,
        signature_id: None,
    };

    // Validating encode — see the doc comment. This is what actually moves
    // ABI encoding and signing in front of the reserve; `process_message`
    // will encode again with its own try index.
    //
    // The error is redacted WHOLE, and neither half of the SDK's is kept.
    // `encode_message` signs, so it reaches `KeyPair::decode`
    // (`abi/signing.rs:35`), and that path emits key material into its
    // message by construction:
    //
    //   * `invalid_public_key` interpolates the public key verbatim —
    //     `format!("Invalid public key [{}]: {}", key, err)`
    //     (`tvm_client/src/crypto/errors.rs:125`);
    //   * `invalid_secret_key` / `invalid_key` interpolate `strip_secret(key)`,
    //     which is the secret's **first 8 characters** plus its length
    //     (`crypto/keys.rs:31-38`).
    //
    // So `e.message()` in `reason` and `{e:?}` in `source` both violate the
    // ticket's unconditional rule — "**никогда не печатает содержимое файла
    // ключей**" — and `source` is the worse of the two, because it is what
    // `--json` serialises and what ends up in logs. Keep the error code,
    // which is a stable classifier and carries nothing, and drop the text.
    let validated = tvm_client::abi::encode_message(context.clone(), encode.clone())
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "compose multisig sendTransaction for {}: the SDK refused to encode or sign the \
                 message (client error {}). Nothing was sent.\n\x20 The underlying text is \
                 withheld deliberately: on this path it can contain the public key or the first \
                 bytes of the secret from --from-keys. Check that the file still holds the pair \
                 preflight approved.",
                from.extended(),
                // A method, not a field: `ClientError` is a newtype over a
                // boxed inner struct with no `Deref` (`tvm_client/src/error.rs:145`).
                e.code(),
            ),
            // Deliberately empty. There is no redacted form of this error
            // worth keeping: the parts that identify the fault are the
            // parts that carry the key.
            source: None,
        })?;

    Ok(ComposedBurn {
        encode,
        bounce,
        amount_micro: amount.0,
        validated_message_id: validated.message_id,
    })
}

/// Broadcast the composed message. Point of no return.
///
/// EVERY failure here is [`CliError::BurnOutcomeUnknown`] (exit 10),
/// including a clean-looking `Ok` whose transaction aborted: once the SDK
/// has been asked to send, "unknown" is the only honest answer, and the
/// caller must not delete the idempotency record on the way out.
///
/// Keeps `process_message` rather than dropping to
/// `send_message` + `wait_for_transaction`. The ABI header carries
/// `expire`, so `process_message`'s loop re-encodes with a fresh expiry
/// and retries a message that timed out in flight; hand-rolling the send
/// would silently drop that. The cost is that the id of the message that
/// actually lands is not knowable here — which is precisely the gap the
/// reconciliation design in Part 5 has to close, and the reason this task
/// does not pretend to close it.
pub async fn send(
    context: &Arc<ClientContext>,
    from: &FromAddress,
    composed: ComposedBurn,
) -> CliResult<BurnReceipt> {
    let ComposedBurn {
        encode,
        bounce,
        amount_micro,
        validated_message_id,
    } = composed;
    debug!(%validated_message_id, "sending composed burn (id will differ per try index)");
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
            "sendTransaction on multisig {}: the SDK reported client error {} — reconcile via \
             GraphQL before retrying.\n\x20 The underlying text is withheld: this path signs, and \
             the SDK's signing errors embed the public key or the first bytes of the secret from \
             --from-keys.",
            from.extended(),
            e.code(),
        ),
        source: None,
    })?;

    // Classify the returned transaction. `process_message` can return Ok
    // even when the compute phase reverted (exit_code != 0), so we must
    // inspect the tx JSON before declaring success.
    let Some((aborted, exit_code)) = classify_tx(&processed.transaction) else {
        return Err(CliError::BurnOutcomeUnknown {
            reason: format!(
                "multisig {} returned a transaction this build cannot classify: it carries \
                 neither `aborted` nor `compute.exit_code`, so nothing in it says the call \
                 succeeded. The burn is on the wire; reconcile via GraphQL before retrying.",
                from.extended()
            ),
            source: None,
        });
    };
    if aborted || exit_code.is_some_and(|c| c != 0) {
        return Err(CliError::BurnOutcomeUnknown {
            reason: format!(
                "multisig sendTransaction aborted (exit_code={exit_code:?}, aborted={aborted}) — \
                 the multisig itself rejected the call before forwarding to USDCBridge; reconcile \
                 via GraphQL"
            ),
            source: None,
        });
    }

    let an_tx_hash = extract_tx_id(&processed.transaction)?;
    Ok(BurnReceipt {
        an_tx_hash,
        sent_amount_micro: amount_micro,
        bounce,
    })
}

// -- Helpers -----------------------------------------------------------

/// Load a tvm-cli-style keys.json into a [`KeyPair`]. File format:
/// `{"public":"<64-hex>","secret":"<64-hex>"}`. Both halves must be
/// exactly 64 lowercase hex chars.
///
/// The returned `KeyPair` is the ONLY place the secret half lives; it is
/// consumed by `Signer::Keys` and dropped when `compose` returns.
fn load_keypair(path: &Path) -> CliResult<KeyPair> {
    let contents = std::fs::read_to_string(path).map_err(|e| CliError::KeyFilePerms {
        path: path.display().to_string(),
        problem: format!("cannot read: {e}"),
    })?;
    let json: Value = serde_json::from_str(&contents).map_err(|_| CliError::Preflight {
        reason: format!("--from-keys {}: not valid JSON", path.display()),
        source: None,
    })?;
    // Normalised the SAME way preflight normalises, via
    // `normalize_u256_hex`, and not merely lowercased.
    //
    // Preflight reads this file through `load_owner_keypair_hex`, which
    // normalises, and compares the result against the on-chain custodian
    // key. `compose` re-reads it and compares again. When the two used
    // different rules, a `0x`-prefixed or unpadded `keys.json` passed
    // preflight and then failed that re-check — permanently, since
    // re-running does the same thing — with "the public key … is no longer
    // the one verified … Nothing was sent. Re-run."
    //
    // It is also what goes to the SDK: `Signer::Keys` gets these strings,
    // so handing it a `0x` prefix would move the failure into the signing
    // path whose error text this module deliberately withholds.
    let half = |field: &str| -> CliResult<String> {
        let raw = json
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::Preflight {
                reason: format!(
                    "--from-keys {}: missing '{field}' string field",
                    path.display()
                ),
                source: None,
            })?;
        crate::preflight::normalize_key_file_hex(raw).ok_or_else(|| CliError::Preflight {
            reason: format!(
                "--from-keys {}: '{field}' field is not 1-64 hex characters (an optional \
                 `0x` prefix is accepted)",
                path.display()
            ),
            source: None,
        })
    };
    let public = half("public")?;
    let secret = half("secret")?;
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
            "encode initiateWithdrawal body: the SDK reported client error {} — check the \
             USDCBridge ABI against --to / --to-chain.",
            e.code()
        ),
        // This call passes `Signer::None`, so today it cannot reach
        // `KeyPair::decode` and cannot leak a key. It is redacted anyway:
        // it is the same shape, in the same file, three calls from one that
        // does sign, and leaving one un-redacted instance is how the
        // pattern comes back.
        source: None,
    })?;
    Ok(encoded.body)
}

/// Extract `.id` (or `.hash`) from the tx JSON returned by
/// `process_message`. Normalizes to `0x`-prefixed 64-hex.
///
/// This string is the operator's only handle on an irreversible burn: it
/// is written to the state file, it is the GraphQL filter that waits for
/// `WithdrawalInitiated`, and it is what every reconciliation instruction
/// the CLI prints tells them to look up. A value that merely LOOKS like a
/// hash is worse than none, so anything outside 1..=64 hex digits is exit
/// 10 — the burn is on the wire either way, and the operator has to know
/// we cannot name it.
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
    let trimmed = raw.trim();
    let bare = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    // `"id": ""` used to left-pad into `0x000…0` and then be reported as
    // "the burn is on the wire, here is its hash". It matched nothing, on
    // every network, forever.
    if bare.is_empty() || bare.len() > 64 || !bare.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::BurnOutcomeUnknown {
            reason: format!(
                "process_message returned a transaction id that is not a hash ({} characters \
                 after the optional 0x, hex-only: {}) — the burn is on the wire but this build \
                 cannot name it; reconcile via GraphQL",
                bare.len(),
                bare.chars().all(|c| c.is_ascii_hexdigit()),
            ),
            source: None,
        });
    }
    // Left-pad if the network returned a shorter id (shellnet quirk), and
    // lowercase on BOTH paths: the GraphQL filter is byte-exact, so a
    // short UPPERCASE id used to build a query that matched nothing and
    // time out five minutes after the money had already moved.
    Ok(format!("0x{:0>64}", bare.to_ascii_lowercase()))
}

/// `(aborted, compute.exit_code)` extraction. Mirrors
/// `acki-nacki-interface::classify_tx_json` but returns just the two
/// fields we need — and, unlike it, refuses to guess.
///
/// The reference classifier reads a missing or malformed field as "not
/// aborted", which is right where it lives: the relayer classifies
/// transactions that already happened, and a wrong guess costs an index
/// entry. Here the same guess is durable and irreversible. A transaction
/// we call fine gets `Status::Burned` and a hash written to the state
/// file, and from then on no run will re-burn without an operator
/// hand-editing that file — so a reverted burn read as fine strands the
/// withdrawal. `None` means "this shape carries no evidence either way",
/// which the caller turns into exit 10 and the operator reconciles.
///
/// This is the module discipline stated at the top of the file: we do
/// not distinguish "not sent" from "sent, waiting" by guessing.
fn classify_tx(tx: &Value) -> Option<(bool, Option<i32>)> {
    // `null` is a field that is not there. Anything else that is not a
    // bool is a shape this build does not understand, and reading it as
    // `false` is the silent failure.
    let aborted = match tx.get("aborted") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return None,
    };

    let raw_exit = tx
        .get("compute")
        .and_then(|c| c.get("exit_code"))
        .filter(|v| !v.is_null())
        .or_else(|| tx.get("exit_code").filter(|v| !v.is_null()));
    let exit_code = match raw_exit {
        None => None,
        // NOT `as i32`: that folds a value which does not fit into a
        // plausible code, and 4294967296 folds to 0, i.e. "success".
        Some(v) => Some(v.as_i64().and_then(|c| i32::try_from(c).ok())?),
    };

    // Neither field present. `process_message` returns the whole
    // transaction, so this is a shape change or a filtered response —
    // not a normal success.
    if aborted.is_none() && exit_code.is_none() {
        return None;
    }
    Some((aborted.unwrap_or(false), exit_code))
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
    fn extract_tx_id_empty_string_is_not_a_hash() {
        // The left-pad used to turn this into 0x000…0 and hand it back as
        // the burn's identity: written to the state file, printed to the
        // operator, fed to the GraphQL filter that then matched nothing.
        for id in ["", "0x", "   ", "0X"] {
            let tx = json!({ "id": id });
            match extract_tx_id(&tx) {
                Err(CliError::BurnOutcomeUnknown {
                    ..
                }) => (),
                other => panic!("{id:?} is not a transaction id, got {other:?}"),
            }
        }
    }

    #[test]
    fn extract_tx_id_non_hex_is_not_a_hash() {
        for id in ["not-a-hash", "0xzz", &"a".repeat(65)] {
            let tx = json!({ "id": id });
            match extract_tx_id(&tx) {
                Err(CliError::BurnOutcomeUnknown {
                    ..
                }) => (),
                other => panic!("{id:?} is not a transaction id, got {other:?}"),
            }
        }
    }

    #[test]
    fn extract_tx_id_lowercases_a_short_id_too() {
        // The `< 64` branch skipped the lowercasing, and the GraphQL
        // filter is byte-exact: an upper-case short id built a query that
        // matched nothing and timed out five minutes after the burn.
        let tx = json!({ "id": "0xABC" });
        assert_eq!(extract_tx_id(&tx).unwrap(), format!("0x{:0>64}", "abc"));
    }

    #[test]
    fn classify_tx_success() {
        let tx = json!({ "aborted": false, "compute": { "exit_code": 0 } });
        assert_eq!(classify_tx(&tx), Some((false, Some(0))));
    }

    #[test]
    fn classify_tx_aborted() {
        let tx = json!({ "aborted": true, "compute": { "exit_code": 108 } });
        assert_eq!(classify_tx(&tx), Some((true, Some(108))));
    }

    #[test]
    fn classify_tx_refuses_to_call_an_unreadable_shape_a_success() {
        // Reading "no evidence" as "not aborted" is not a neutral default
        // here: it writes Status::Burned plus a hash, and from then on no
        // run re-burns without the operator hand-editing the state file.
        // So a reverted burn read as fine strands the withdrawal.
        for tx in [
            json!({}),
            json!({ "aborted": null, "compute": { "exit_code": null } }),
            // Present but not a bool — a shape change, not a success.
            json!({ "aborted": "false" }),
            json!({ "aborted": 0 }),
            // Would fold to 0 under `as i32`, i.e. to "success".
            json!({ "compute": { "exit_code": 4294967296i64 } }),
        ] {
            assert_eq!(classify_tx(&tx), None, "unclassifiable: {tx}");
        }
    }

    #[test]
    fn classify_tx_reads_one_field_when_that_is_all_there_is() {
        // Refusing must not become refusing everything: a transaction that
        // carries either field is classifiable from it.
        assert_eq!(classify_tx(&json!({ "aborted": true })), Some((true, None)));
        assert_eq!(
            classify_tx(&json!({ "aborted": false })),
            Some((false, None))
        );
        assert_eq!(
            classify_tx(&json!({ "exit_code": 0 })),
            Some((false, Some(0)))
        );
        assert_eq!(
            classify_tx(&json!({ "exit_code": 108 })),
            Some((false, Some(108)))
        );
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

    // -- compose / send ---------------------------------------------------

    /// Fixtures. `burn.rs` needs its own copies of `sample_from`/`sample_to` —
    /// test modules do not share scope with `idempotency.rs`'s. The key pair
    /// comes from `crate::test_keys`, which is the crate's single copy: two
    /// drifting copies of a real pair produce a keys.json whose halves do not
    /// match, and that fails inside the SDK, in the one error path this crate
    /// deliberately withholds (F18).
    use crate::test_keys::{PAIR_PUBLIC, PAIR_SECRET};

    fn sample_from() -> FromAddress {
        FromAddress {
            dapp_id_hex: "ab".repeat(32),
            account_id_hex: "cd".repeat(32),
        }
    }

    fn sample_to() -> ToAddress {
        ToAddress {
            address: "0x742d35Cc6634C0532925a3b844Bc454e4438f44e"
                .parse()
                .unwrap(),
            chain_id: 11155111,
        }
    }

    fn sample_preflight() -> PreflightReport {
        // The two id byte arrays and the two display strings must describe the
        // same values — `preflight.rs` has a test that asserts exactly that,
        // and a fixture that violates it would fail for the wrong reason.
        let dapp_id = [0u8; 32];
        let account_id = [0x1au8; 32];
        PreflightReport {
            from: sample_from(),
            to: sample_to(),
            amount: UsdcAmount(1_000_000),
            usdc_bridge_extended: format!("{}::{}", hex::encode(dapp_id), hex::encode(account_id)),
            usdc_bridge_legacy: format!("0:{}", hex::encode(account_id)),
            bridge_dapp_id: dapp_id,
            bridge_account_id: account_id,
            multisig_ecc3_balance: 1_000_000,
            owner_pubkey_hex: PAIR_PUBLIC.to_string(),
        }
    }

    /// A 0400 keys.json holding the pinned pair. Kept in a `OnceLock` so the
    /// `TempDir` outlives every test that borrows the path.
    fn sample_keys_path() -> &'static Path {
        use std::sync::OnceLock;
        static KEYS: OnceLock<(tempfile::TempDir, std::path::PathBuf)> = OnceLock::new();
        let (_dir, path) = KEYS.get_or_init(|| {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("keys.json");
            std::fs::write(
                &path,
                format!(r#"{{"public":"{PAIR_PUBLIC}","secret":"{PAIR_SECRET}"}}"#),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
            (dir, path)
        });
        path
    }

    /// A client context that never reaches a network. `ClientContext::new`
    /// only builds config — it does not connect — and every step `compose`
    /// performs (ABI body encoding, message encoding, signing) is local, so
    /// this exercises the real `compose` rather than a stand-in.
    fn offline_ctx() -> Arc<ClientContext> {
        let config = tvm_client::ClientConfig {
            network: tvm_client::net::NetworkConfig {
                endpoints: Some(vec!["https://example.invalid/graphql".into()]),
                sending_endpoint_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::new(ClientContext::new(config).unwrap())
    }

    #[tokio::test]
    async fn compose_rejects_a_keyless_file_before_anything_is_sent() {
        // Must call `compose` itself, not one of its helpers. The guarantee
        // this task rests on is that COMPOSE fails — asserting on
        // `load_keypair` directly would still pass if someone rewrote
        // `compose` to defer key loading into `send`.
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"{{"public":"{}"}}"#, "a".repeat(64)).unwrap();

        let err = compose(
            &offline_ctx(),
            &sample_preflight(),
            &sample_from(),
            &path,
            &sample_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .await
        .expect_err("a keys.json with no secret must be refused by compose");
        assert!(
            !matches!(err, CliError::BurnOutcomeUnknown { .. }),
            "a key-file problem must never be reported as an ambiguous burn, got {err:?}",
        );
    }

    #[tokio::test]
    async fn a_0x_prefixed_keys_file_still_composes() {
        // The permanent dead end. Preflight normalised the file's halves;
        // `load_keypair` only lowercased them. So a `0x`-prefixed or
        // unpadded keys.json passed preflight and then failed `compose`'s
        // re-check against the very key preflight had just approved —
        // "the public key … is no longer the one verified … Re-run." — on
        // every attempt, forever. Same file, same command, same failure.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        // Upper case AND a `0x` prefix: the two shapes lowercasing alone
        // cannot reconcile.
        std::fs::write(
            &path,
            format!(
                r#"{{"public":"0x{}","secret":"0x{}"}}"#,
                PAIR_PUBLIC.to_ascii_uppercase(),
                PAIR_SECRET.to_ascii_uppercase(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();

        let composed = compose(
            &offline_ctx(),
            &sample_preflight(),
            &sample_from(),
            &path,
            &sample_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .await
        .expect("a 0x-prefixed keys.json is the same key and must compose");
        assert_eq!(composed.validated_message_id.len(), 64);
    }

    #[test]
    fn an_all_decimal_key_stays_hex() {
        // `normalize_u256_hex` reads an all-digit string as DECIMAL, which
        // is right for a uint256 off the chain and wrong for a key file.
        // Reusing it here would silently turn this perfectly ordinary hex
        // key into a different 256-bit value — a worse bug than the one the
        // normalisation was added to fix.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        let digits = "1".repeat(64);
        std::fs::write(
            &path,
            format!(r#"{{"public":"{digits}","secret":"{digits}"}}"#),
        )
        .unwrap();
        let kp = load_keypair(&path).unwrap();
        assert_eq!(kp.public, digits, "an all-digit key is hex, not decimal");
        assert_eq!(kp.secret, digits);
    }

    #[tokio::test]
    async fn compose_actually_encodes_so_send_has_no_local_failure_left() {
        // The load-bearing property. `process_message` re-encodes internally
        // (tvm_client/src/processing/process_message.rs:45), so composing only
        // the *params* would leave ABI encoding and signing to run AFTER the
        // reserve — exactly the window this task exists to close. Performing
        // the encode here proves the params and the key can produce a message
        // at all, before any state is written.
        let composed = compose(
            &offline_ctx(),
            &sample_preflight(),
            &sample_from(),
            sample_keys_path(),
            &sample_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .await
        .expect("a well-formed request must compose offline");
        assert_eq!(
            composed.validated_message_id.len(),
            64,
            "compose must have run a real encode_message",
        );
    }

    #[tokio::test]
    async fn a_signing_failure_never_echoes_the_key_file() {
        // A well-formed keys.json whose halves do not belong together. It
        // passes `load_keypair` and fails inside `encode_message`, which is
        // the SDK path that interpolates key material into its message
        // (`crypto/errors.rs:118-126`).
        //
        // The public half MUST be `PAIR_PUBLIC`. `compose` re-checks it
        // against `preflight.owner_pubkey_hex` before encoding (the TOCTOU
        // guard), so a made-up public key stops the test one branch too
        // early — it would pass while proving nothing about the SDK's
        // signing errors, which is the whole point.
        //
        // The secret is what gets corrupted: a valid-looking 64-hex value
        // that is not this public key's partner. `load_keypair` accepts it,
        // and `encode_message` fails inside `KeyPair::decode`.
        const BAD_SECRET: &str = "bb";
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("mismatched.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"public":"{PAIR_PUBLIC}","secret":"{}"}}"#,
                BAD_SECRET.repeat(32)
            ),
        )
        .unwrap();

        let err = compose(
            &offline_ctx(),
            &sample_preflight(),
            &sample_from(),
            &path,
            &sample_to(),
            &UsdcAmount(1_000_000),
            true,
        )
        .await
        .expect_err("a mismatched pair cannot be signed");

        // Confirm we reached the SDK, not the TOCTOU guard. Without this the
        // test silently degrades into a second copy of the guard's own test
        // the moment someone changes the fixture.
        let msg = format!("{err}");
        assert!(
            msg.contains("client error"),
            "must fail inside encode/sign, not at the public-key guard: {msg}"
        );

        let rendered = format!("{err} {err:?}");
        for leaked in [
            PAIR_PUBLIC.to_string(),
            BAD_SECRET.repeat(32),
            // `strip_secret` shows the first EIGHT characters; asserting only
            // on full-length strings would pass with a usable prefix leaked.
            BAD_SECRET.repeat(4),
        ] {
            assert!(
                !rendered.contains(&leaked),
                "the refusal leaked key material: {rendered}"
            );
        }
    }

    /// Compile-only guard against a merge collapsing `compose` and `send` back
    /// into one `fire`. That shape is what forced the idempotency reserve to
    /// sit upstream of fallible pre-send work — the whole bug in F4.
    ///
    /// Never called; it exists so the file fails to build if either function
    /// disappears or changes shape. Written as a body that calls them rather
    /// than as fn-pointer coercions: an `async fn` returns an opaque future
    /// whose lifetimes are tied to its arguments, so
    /// `let _: fn(&A, &B) -> _ = compose;` does not compile
    /// (E0308, "one type is more general than the other").
    #[allow(dead_code)]
    async fn _compose_then_send_signature_guard(
        ctx: &Arc<ClientContext>,
        preflight: &PreflightReport,
        from: &FromAddress,
        from_keys: &Path,
        to: &ToAddress,
        amount: &UsdcAmount,
    ) {
        let composed = compose(ctx, preflight, from, from_keys, to, amount, true)
            .await
            .expect("signature guard only");
        let _ = send(ctx, from, composed).await;
    }
}
