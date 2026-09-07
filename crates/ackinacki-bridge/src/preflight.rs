//! Read-only preflight checks. No signing, no sends, no state writes.
//!
//! Runs after arg validation and before any broadcast. Every failure here
//! surfaces as [`crate::errors::CliError::Preflight`] and exit code 2.
//!
//! Ordering matters — cheap and local checks first (perms), then GraphQL
//! (account state), then RPC-shaped (multisig introspection). We stop at
//! the first failure so the user's first error message names the thing
//! that's actually blocking them, not a downstream symptom.
//!
//! Everything runs in-process via the tvm-sdk (`tvm_client`, `tvm_block`).
//! No `tvm-cli` binary shell-out: keys/args never enter argv, errors are
//! typed instead of string-scraped, and the ABI JSON is embedded at build
//! time rather than staged to a temp file. `bridge_gql_fetcher::GqlClient`
//! is reused for the one direct GraphQL query we make (USDCBridge dapp_id
//! resolution) — the daemon already uses it, so we inherit its shape.

use std::path::Path;
use std::sync::Arc;

use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use bridge_gql_fetcher::gql_client::{create_client, GqlClient};
use bridge_relayer_daemon::bridge::EthBridgeClient;
use serde_json::{json, Value};
use tvm_block::{Account, AccountStatus, Deserializable};
use tvm_client::abi::{
    encode_message, Abi, CallSet, ParamsOfEncodeMessage, Signer,
};
use tvm_client::account::{get_account, ParamsOfGetAccount};
use tvm_client::net::NetworkConfig;
use tvm_client::tvm::{run_tvm, ParamsOfRunTvm};
use tvm_client::{ClientConfig, ClientContext};

use crate::args::{self, FromAddress, ToAddress, UsdcAmount};
use crate::errors::{CliError, CliResult};

/// Vendored multisig ABI. Embedded at build time so the binary has no
/// runtime companion directory; passed to `tvm_client` as `Abi::Json(...)`.
const MULTISIG_ABI_JSON: &str = include_str!("../abi/UpdateCustodianMultisigWallet.abi.json");

/// Everything preflight learned that later stages want to consume without
/// re-querying: the multisig's on-chain state, the resolved live
/// USDCBridge address (dapp_id::account_id), the multisig's current ECC[3]
/// balance for the record-keeping.
#[derive(Debug, Clone)]
pub struct PreflightReport {
    pub from: FromAddress,
    pub to: ToAddress,
    pub amount: UsdcAmount,
    pub usdc_bridge_extended: String,
    pub usdc_bridge_legacy: String,
    /// The same two ids as `usdc_bridge_extended`, decoded once.
    ///
    /// Typed because the only consumer is `withdrawal_identity_frs`, and a
    /// `String` there would mean parsing hex at the call site — the exact
    /// place a stray `0x`, an odd length, or a helpful `.rev()` turns into
    /// a preflight that compares the wrong number and passes.
    ///
    /// Byte order is `hex::decode`'s, matching `parse_hex_array::<32>` in
    /// `bridge-event-prover-lib/src/prover.rs:191`. The proof and this
    /// check must agree, and they agree by using the same decoding of the
    /// same hex string.
    pub bridge_dapp_id: [u8; 32],
    pub bridge_account_id: [u8; 32],
    pub multisig_ecc3_balance: u128,
    pub owner_pubkey_hex: String,
}

/// Whether the ECC[3] sufficiency comparison still applies.
///
/// Deliberately not a bare `bool` at the call site: `run(.., true)` reads
/// as "check the balance" to half of readers and "skip it" to the other
/// half, and getting it backwards silently disables a money check.
#[derive(Clone, Copy, Debug)]
pub enum BalanceCheck {
    /// No burn has been broadcast for this identity. The multisig must
    /// hold at least `amount`.
    Require,
    /// A prior run already broadcast the burn (`an_tx_hash` on file). The
    /// balance is expected to be short — that is what success looks like.
    SkipSpent,
}

impl BalanceCheck {
    pub fn from_burn_sent(sent: bool) -> Self {
        if sent {
            Self::SkipSpent
        } else {
            Self::Require
        }
    }
}

/// Run every preflight check in order and return the aggregated report.
///
/// Sequence:
/// 1. `--from-keys` file permissions (already checked by args::validate,
///    re-checked here so `preflight` is self-contained for callers that skip
///    arg validation).
/// 2. `--from` account exists on AN, is `Active`, has non-zero code hash.
/// 3. `--from` is a multisig (typed `getCustodians` call succeeds).
/// 4. `custodianCount == 1` (single-custodian invariant; multi-owner sends
///    would need submit+confirm from other owners → contract exit 108).
/// 5. Owner pubkey decoded from `--from-keys` matches multisig's on-chain owner
///    pubkey.
/// 6. USDCBridge live dapp_id resolved via GraphQL (canonical account default
///    overridden by `--usdc-bridge-account`).
/// 7. Multisig ECC[3] balance ≥ amount (preflight, not guarantee — the chain
///    has the final word between check and send). Skipped, and only skipped,
///    when `balance_check` says the burn is already on the wire: see
///    [`BalanceCheck`]. The other six always run — a resume that skipped them
///    would be a different command wearing the same name.
pub async fn run(
    from: &FromAddress,
    from_keys: &Path,
    to: &ToAddress,
    amount: &UsdcAmount,
    gql_endpoint: &str,
    usdc_bridge_account_id_hex: &str,
    balance_check: BalanceCheck,
) -> CliResult<PreflightReport> {
    // 1. File perms (belt-and-suspenders — args::validate already ran this).
    args::check_key_file_perms(from_keys)?;

    // Shared tvm-sdk client — used for account fetch + local getter exec.
    let context = build_client_context(gql_endpoint)?;

    // 2. Fetch --from account BOC, parse via tvm_block::Account, verify
    //    Active + has code_hash.
    let account = fetch_account(&context, &from.dapp_id_hex, &from.account_id_hex).await?;
    let account_boc =
        fetch_account_boc(&context, &from.dapp_id_hex, &from.account_id_hex).await?;
    let acc_type = describe_account_status(account.status());
    if account.status() != AccountStatus::AccStateActive {
        return Err(CliError::Preflight {
            reason: format!(
                "--from account {}: acc_type is {acc_type} (need Active)",
                from.extended()
            ),
            source: None,
        });
    }
    if account.get_code_hash().is_none() {
        return Err(CliError::Preflight {
            reason: format!(
                "--from account {}: no code_hash (Active but codeless — impossible without corruption)",
                from.extended()
            ),
            source: None,
        });
    }

    // 3+4+5. run_tvm(getCustodians) → parse → count == 1 → pubkey match.
    // Encode+run_tvm needs the legacy `0:<acc>` form — tvm-sdk v3.0.5.an's
    // ABI message encoder rejects the v3 `dapp_id::acc_id` shape here (works
    // for `get_account` above and for tvm-cli `--addr`, but not for the
    // `address` field on `ParamsOfEncodeMessage`). See memory
    // `tvm_cli_v3_address_forms.md`.
    let custodians = call_get_custodians(&context, &from.legacy(), &account_boc).await?;
    if custodians.len() != 1 {
        return Err(CliError::Preflight {
            reason: format!(
                "--from account {}: single-custodian multisig required, found {} custodians. \
                 This CLI's sendTransaction path signs alone and cannot satisfy multi-owner \
                 confirmation (would fail with contract exit 108 on-chain).",
                from.extended(),
                custodians.len()
            ),
            source: None,
        });
    }
    let on_chain_pubkey = custodians[0].owner_pubkey_hex.as_deref().ok_or_else(|| {
        CliError::Preflight {
            reason: format!(
                "--from account {}: custodian 0 has no owner_pubkey — this CLI does not \
                 support address-owned custodians (only pubkey-owned).",
                from.extended()
            ),
            source: None,
        }
    })?;
    let (local_pubkey, _secret) = load_owner_keypair_hex(from_keys)?;
    if !pubkeys_equal(&local_pubkey, on_chain_pubkey) {
        // Do NOT echo the local key back — even the public half is a stable
        // identifier that a user might not want in shell history. The
        // on-chain half is public; showing that alone is enough to guide
        // "you gave me the wrong keys.json".
        return Err(CliError::Preflight {
            reason: format!(
                "--from-keys does not match on-chain owner of {}: expected pubkey {} (from getCustodians)",
                from.extended(),
                on_chain_pubkey
            ),
            source: None,
        });
    }

    // 6. USDCBridge lookup: resolve dapp_id AND verify the account is Active.
    //    One GQL round-trip through the same helper the daemon uses.
    let gql = create_client(gql_endpoint).map_err(|e| CliError::Preflight {
        reason: format!("failed to build GraphQL client for {gql_endpoint}: {e}"),
        source: Some(e),
    })?;
    let (usdc_bridge_dapp_id, usdc_bridge_acc_type) =
        query_usdc_bridge_state(&gql, usdc_bridge_account_id_hex).await?;
    if usdc_bridge_acc_type != "Active" {
        return Err(CliError::Preflight {
            reason: format!(
                "USDCBridge account {usdc_bridge_account_id_hex}: acc_type is \
                 {usdc_bridge_acc_type} (need Active) — is the bridge deployed?"
            ),
            source: None,
        });
    }
    let usdc_bridge_extended = format!("{usdc_bridge_dapp_id}::{usdc_bridge_account_id_hex}");
    let usdc_bridge_legacy = format!("0:{usdc_bridge_account_id_hex}");

    // Decode once, here, where both hex strings are in hand and already
    // known good — `query_usdc_bridge_state` normalises the zerostate
    // empty dapp_id to 64 zeros, and the account id was validated by
    // `args`. A helper keeps the two identical; hand-rolling the second
    // one is how they end up differing.
    let decode_id = |label: &str, hex_str: &str| -> CliResult<[u8; 32]> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(hex_str, &mut out).map_err(|e| CliError::Preflight {
            reason: format!("{label}: expected 64 hex chars, got {hex_str:?} ({e})"),
            source: None,
        })?;
        Ok(out)
    };
    let bridge_dapp_id = decode_id("USDCBridge dapp_id", &usdc_bridge_dapp_id)?;
    let bridge_account_id = decode_id("USDCBridge account_id", usdc_bridge_account_id_hex)?;

    // 7. ECC[3] balance from the parsed account.
    //
    // The balance is ALWAYS fetched and always reported — it is in the
    // `PreflightReport` and in the log line, and an operator reconciling a
    // resumed run wants to see it. Only the refusal is conditional.
    let ecc3_balance = extract_ecc_balance(&account, 3)?;
    match balance_check {
        BalanceCheck::Require if ecc3_balance < amount.0 => {
            return Err(CliError::Preflight {
                reason: format!(
                    "--from account {}: ECC[3] balance {} µUSDC ({}) < requested {} µUSDC ({}). \
                     (ECC[3] is USDC; ECC[2] is Shell — do not confuse them.)",
                    from.extended(),
                    ecc3_balance,
                    UsdcAmount(ecc3_balance).display(),
                    amount.0,
                    amount.display(),
                ),
                source: None,
            });
        },
        BalanceCheck::Require => {},
        BalanceCheck::SkipSpent => {
            tracing::info!(
                ecc3_balance,
                requested = amount.0,
                "resume: skipping the ECC[3] sufficiency check — the burn for this withdrawal is \
                 already on the wire, so a short balance is expected",
            );
        },
    }

    Ok(PreflightReport {
        from: from.clone(),
        to: *to,
        amount: *amount,
        usdc_bridge_extended,
        usdc_bridge_legacy,
        bridge_dapp_id,
        bridge_account_id,
        multisig_ecc3_balance: ecc3_balance,
        owner_pubkey_hex: on_chain_pubkey.to_string(),
    })
}

// -- tvm-sdk client construction ---------------------------------------------

fn build_client_context(gql_endpoint: &str) -> CliResult<Arc<ClientContext>> {
    let config = ClientConfig {
        network: NetworkConfig {
            endpoints: Some(vec![gql_endpoint.to_string()]),
            sending_endpoint_count: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let ctx = ClientContext::new(config).map_err(|e| CliError::Preflight {
        reason: format!("failed to build tvm_client context for {gql_endpoint}: {e}"),
        source: Some(anyhow::anyhow!("{e}")),
    })?;
    Ok(Arc::new(ctx))
}

// -- Account fetch + parse (checks 2 + 7) ------------------------------------

async fn fetch_account_boc(
    ctx: &Arc<ClientContext>,
    dapp_id_hex: &str,
    account_id_hex: &str,
) -> CliResult<String> {
    let params = ParamsOfGetAccount {
        account_id: account_id_hex.to_string(),
        dapp_id: dapp_id_hex.to_string(),
    };
    let r = get_account(ctx.clone(), params).await.map_err(|e| CliError::Preflight {
        reason: format!(
            "fetch account {dapp_id_hex}::{account_id_hex}: {}",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;
    if r.boc.is_empty() {
        return Err(CliError::Preflight {
            reason: format!(
                "account {dapp_id_hex}::{account_id_hex}: not on chain (empty BOC)"
            ),
            source: None,
        });
    }
    Ok(r.boc)
}

async fn fetch_account(
    ctx: &Arc<ClientContext>,
    dapp_id_hex: &str,
    account_id_hex: &str,
) -> CliResult<Account> {
    let boc = fetch_account_boc(ctx, dapp_id_hex, account_id_hex).await?;
    Account::construct_from_base64(&boc).map_err(|e| CliError::Preflight {
        reason: format!(
            "parse account BOC for {dapp_id_hex}::{account_id_hex}: {e}"
        ),
        source: Some(anyhow::anyhow!("{e}")),
    })
}

fn describe_account_status(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::AccStateActive => "Active",
        AccountStatus::AccStateUninit => "Uninit",
        AccountStatus::AccStateFrozen => "Frozen",
        AccountStatus::AccStateNonexist => "NonExist",
    }
}

// -- getCustodians (checks 3-5) ----------------------------------------------

/// One custodian from `getCustodians` output.
#[derive(Debug, Clone)]
struct Custodian {
    /// Normalized to 64-char lowercase hex if present, `None` if the
    /// custodian is address-owned.
    owner_pubkey_hex: Option<String>,
}

async fn call_get_custodians(
    ctx: &Arc<ClientContext>,
    address_extended: &str,
    account_boc: &str,
) -> CliResult<Vec<Custodian>> {
    let abi = Abi::Json(MULTISIG_ABI_JSON.to_string());

    // Encode an empty-input getCustodians call as an unsigned external
    // message aimed at the multisig. This is the same shape tvm-cli's
    // `runx` builds under the hood.
    let encoded = encode_message(
        ctx.clone(),
        ParamsOfEncodeMessage {
            abi: abi.clone(),
            address: Some(address_extended.to_string()),
            call_set: Some(CallSet {
                function_name: "getCustodians".to_string(),
                header: None,
                input: Some(json!({})),
            }),
            signer: Signer::None,
            deploy_set: None,
            processing_try_index: None,
            signature_id: None,
        },
    )
    .await
    .map_err(|e| CliError::Preflight {
        reason: format!(
            "encode getCustodians call for {address_extended}: {}",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;

    let run = run_tvm(
        ctx.clone(),
        ParamsOfRunTvm {
            message: encoded.message,
            account: account_boc.to_string(),
            abi: Some(abi),
            execution_options: None,
            boc_cache: None,
            return_updated_account: Some(false),
        },
    )
    .await
    .map_err(|e| CliError::Preflight {
        reason: format!(
            "run_tvm getCustodians on {address_extended}: {} — is this address a deployed multisig?",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;

    let decoded = run.decoded.and_then(|d| d.output).ok_or_else(|| CliError::Preflight {
        reason: format!(
            "run_tvm getCustodians on {address_extended}: no decoded output (ABI mismatch?)"
        ),
        source: None,
    })?;
    parse_custodians(&decoded)
}

fn parse_custodians(json: &Value) -> CliResult<Vec<Custodian>> {
    let arr = json.get("custodians").and_then(|v| v.as_array()).ok_or_else(|| {
        CliError::Preflight {
            reason: format!("getCustodians output missing `custodians` array: {json}"),
            source: None,
        }
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let owner_pubkey_hex = match entry.get("owner_pubkey") {
            Some(Value::Null) | None => None,
            Some(v) => {
                let s = v.as_str().ok_or_else(|| CliError::Preflight {
                    reason: format!(
                        "getCustodians[{i}].owner_pubkey: expected string, got {v}"
                    ),
                    source: None,
                })?;
                Some(normalize_u256_hex(s).ok_or_else(|| CliError::Preflight {
                    reason: format!(
                        "getCustodians[{i}].owner_pubkey: not a valid uint256: {s}"
                    ),
                    source: None,
                })?)
            }
        };
        out.push(Custodian { owner_pubkey_hex });
    }
    Ok(out)
}

// -- USDCBridge GQL lookup (check 6) -----------------------------------------

/// Combined dapp_id + acc_type query for USDCBridge. Zerostate-deployed
/// contracts return an empty string for dapp_id — we normalize to
/// 64 zero-hex so downstream string interpolation is uniform.
async fn query_usdc_bridge_state(
    gql: &GqlClient,
    account_id_hex: &str,
) -> CliResult<(String, String)> {
    let zero64 = "0".repeat(64);
    // Ask for `acc_type_name` (string: "Active"/"Uninit"/…) — the raw
    // `acc_type` field is the underlying integer enum (1 = Active), which
    // the older string-typed match below rejected as "Unknown".
    let q = format!(
        r#"{{ blockchain {{ account(account_id: "{account_id_hex}", dapp_id: "{zero64}") {{ info {{ dapp_id acc_type_name }} }} }} }}"#
    );
    let data = gql.query(&q).await.map_err(|e| CliError::Preflight {
        reason: format!("query USDCBridge {account_id_hex} via GraphQL: {e}"),
        source: Some(e),
    })?;
    let account = data.pointer("/blockchain/account").unwrap_or(&Value::Null);
    if account.is_null() {
        return Err(CliError::Preflight {
            reason: format!(
                "USDCBridge account {account_id_hex}: GraphQL returned no account node — \
                 wrong --usdc-bridge-account, or wrong --gql-endpoint?"
            ),
            source: None,
        });
    }
    let info = account.get("info").unwrap_or(&Value::Null);
    if info.is_null() {
        return Err(CliError::Preflight {
            reason: format!(
                "USDCBridge account {account_id_hex}: GraphQL account.info missing (schema drift?)"
            ),
            source: None,
        });
    }
    let acc_type = info
        .get("acc_type_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let raw_dapp = info
        .get("dapp_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let dapp_id = if raw_dapp.is_empty() { zero64 } else { raw_dapp };
    Ok((dapp_id, acc_type))
}

// -- ECC balance extraction (check 7) ----------------------------------------

/// Pull ECC[<id>] from a parsed account. Uninit / balance-less accounts →
/// 0. Non-existent map key → 0. Follows the same iteration pattern as
/// `tvm-cli account` (`tvm-sdk/tvm_cli/src/account.rs`).
fn extract_ecc_balance(account: &Account, ecc_id: u32) -> CliResult<u128> {
    let bal = match account.balance() {
        Some(b) => b,
        None => return Ok(0),
    };
    // ExtraCurrencyCollection is a HashmapE<u32, VarUInteger32>. `.get()`
    // deserializes the value directly; iteration would work too but a
    // point lookup is clearer for a single ECC id.
    let val = bal.other.get(&ecc_id).map_err(|e| CliError::Preflight {
        reason: format!("read ECC[{ecc_id}] from account: {e}"),
        source: Some(anyhow::anyhow!("{e}")),
    })?;
    let Some(v) = val else {
        return Ok(0);
    };
    // VarUInteger32.value() -> num_bigint::BigInt; decimal string parse
    // to u128. USDC amounts are far below u128::MAX.
    let s = v.value().to_string();
    s.parse::<u128>().map_err(|_| CliError::Preflight {
        reason: format!("ECC[{ecc_id}] value {s} does not fit in u128"),
        source: None,
    })
}

// -- Owner key normalization + comparison ------------------------------------

/// Read `--from-keys` and return both halves, normalised. Preflight needs
/// the public half for the owner match; it validates the secret half too so
/// a truncated or hand-edited keys.json is refused HERE rather than at
/// stage 3, after the confirmation prompt and the idempotency reserve.
///
/// The secret is returned as hex and immediately dropped by the caller —
/// it is never logged, never stored, and never placed in an error.
fn load_owner_keypair_hex(path: &Path) -> CliResult<(String, String)> {
    let contents = std::fs::read_to_string(path).map_err(|e| CliError::KeyFilePerms {
        path: path.display().to_string(),
        problem: format!("cannot read: {e}"),
    })?;
    let json: Value = serde_json::from_str(&contents).map_err(|_| CliError::Preflight {
        reason: format!("--from-keys {}: not valid JSON", path.display()),
        source: None,
    })?;

    let half = |field: &str| -> CliResult<String> {
        let raw = json
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::Preflight {
                reason: format!(
                    "--from-keys {}: missing '{field}' string field — a keys.json must carry both \
                     'public' and 'secret'",
                    path.display()
                ),
                source: None,
            })?;
        normalize_u256_hex(raw).ok_or_else(|| CliError::Preflight {
            reason: format!(
                "--from-keys {}: '{field}' field is not a valid uint256",
                path.display()
            ),
            source: None,
        })
    };

    let public = half("public")?;
    let secret = half("secret")?;

    // Both halves parse — now prove they belong together. `KeyPair::decode`
    // derives the verifying key from the secret and compares it to the
    // declared public. Skipping this lets a mismatched file sail through
    // preflight AND through the on-chain owner comparison above (which only
    // reads `public`). It would still be caught before the wire — the SDK
    // calls this same `decode` while signing — but by then it is reported
    // as BurnOutcomeUnknown (exit 10), telling the operator to reconcile a
    // transaction that never existed.
    //
    // The SDK's error text embeds the public key; we drop it and emit our
    // own, because no part of the key file goes into an error, a log, or
    // --json output.
    tvm_client::crypto::KeyPair::new(public.clone(), secret.clone())
        .decode()
        .map_err(|_| CliError::Preflight {
            reason: format!(
                "--from-keys {}: 'public' and 'secret' do not form a key pair (the public key \
                 derived from 'secret' is a different one) — this file cannot sign for any \
                 multisig",
                path.display()
            ),
            source: None,
        })?;

    Ok((public, secret))
}

/// Coerce a `uint256` from any of {`0x<hex>`, `<hex>`, `<decimal>`} into
/// 64-char lowercase hex with leading zeros preserved.
fn normalize_u256_hex(raw: &str) -> Option<String> {
    use alloy_primitives::U256;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let v = if let Some(rest) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        U256::from_str_radix(rest, 16).ok()?
    } else if raw.chars().all(|c| c.is_ascii_digit()) {
        U256::from_str_radix(raw, 10).ok()?
    } else if raw.chars().all(|c| c.is_ascii_hexdigit()) {
        U256::from_str_radix(raw, 16).ok()?
    } else {
        return None;
    };
    Some(format!("{v:064x}"))
}

pub(crate) fn pubkeys_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// -- EVM-side preflight ------------------------------------------------------

/// The verifier bytecode `aggregate-proof` self-checks its output against.
/// It joins `{name}.bin` onto `--verifiers-dir` (`aggregate_proof.rs:97`).
///
/// `pub(crate)` because Task 8's `check_verifier_bin` uses the same const.
pub(crate) const WITHDRAW_VERIFIER_BIN: &str = "BridgeWithdrawalAggregatorVerifier.bin";

/// Parse `--eth-private-key` into a signer WITHOUT touching the network.
/// Split out from [`check_destination_chain`] so the shape check is
/// unit-testable and so a typo'd key is refused on pure CPU.
///
/// The error deliberately carries no part of the input and no text from
/// the parser. Even a malformed private key is key-shaped material, and
/// hex decoders are helpful in exactly the wrong way: alloy's reports the
/// offending character and its position, which puts a byte of the key —
/// and its offset — into a message bound for logs, shell history and
/// `--json` stdout. Length is all the operator needs to spot a truncated
/// paste; the parser's opinion is dropped on purpose.
pub fn parse_eth_signer(raw: &str) -> CliResult<PrivateKeySigner> {
    raw.parse::<PrivateKeySigner>()
        .map_err(|_| CliError::ArgInvalid {
            flag: "eth-private-key",
            // Say what is accepted, not what looks tidiest. Alloy's
            // `FromStr` goes through `hex::decode_to_array`, which takes
            // the `0x` prefix as optional
            // (`alloy-signer-local-2.4.1/src/private_key.rs:234`), so a
            // bare 64-hex key parses. Claiming "0x-prefixed" would
            // describe a rule this code does not enforce — and the next
            // reader would either add the rule or trust the message.
            expected: "32-byte hex EVM private key, 64 hex chars, `0x` prefix optional".into(),
            got: format!("<redacted {} chars>", raw.len()),
        })
}

/// Preflight the destination chain: the RPC answers, and the chain it
/// reports is the chain the operator named in `--to-chain`.
///
/// Deliberately key-free, so `--dry-run` runs it too. This used to live in
/// stage 6, which meant a wrong RPC was discovered AFTER the irreversible
/// AN burn and up to ~101 min of waiting. The ticket is explicit that the
/// cost of a wrong destination chain is irreversible, so the check belongs
/// before the burn — and a preflight that cannot see it is not much of a
/// preflight.
pub async fn check_destination_chain(rpc_url: &str, expected_chain_id: u64) -> CliResult<()> {
    let url = rpc_url.parse().map_err(|e| CliError::ArgInvalid {
        flag: "rpc-url",
        expected: "an http(s) JSON-RPC URL".into(),
        got: format!("{rpc_url} ({e})"),
    })?;
    let provider = ProviderBuilder::new().connect_http(url);
    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!("--rpc-url {rpc_url}: eth_chainId failed: {e}"),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    if chain_id != expected_chain_id {
        return Err(CliError::Preflight {
            reason: format!(
                "--rpc-url reports chain {chain_id} but --to-chain says {expected_chain_id} — \
                 refusing before the burn, because a withdrawal aimed at the wrong chain cannot \
                 be undone"
            ),
            source: None,
        });
    }
    Ok(())
}

/// Strip the 32-byte CREATE prelude `gen_evm_verifier_shplonk` emits, so
/// what remains is what `eth_getCode` returns for the deployed verifier.
///
/// Split out and unit-tested because an off-by-one here would make every
/// deploy look wrong, which reads as "the check is broken" and gets the
/// check disabled.
pub fn expected_verifier_runtime(bin: &[u8]) -> CliResult<&[u8]> {
    bin.get(32..)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| CliError::Preflight {
            reason: format!(
                "verifier bin is {} bytes — too short to be a CREATE prelude plus a runtime \
                 payload. Re-fetch or rebuild it.",
                bin.len()
            ),
            source: None,
        })
}

/// The `(dappFr, accFr)` pair a proof for this bridge account will carry.
///
/// Same conversion as `bridge-event-prover-lib/src/prover.rs:232-233`, via
/// the same function, so the two cannot drift.
///
/// **Never names `Fr`.** `gosh_dense_balanced_tree` uses `Fr` internally but
/// does not re-export it, and reaching it through
/// `halo2_base::halo2_proofs::halo2curves::bn256::Fr` would make `halo2_base`
/// a direct dependency of this CLI to spell one type. The crate's public
/// surface is exactly the pair `bytes_to_fr` / `fr_to_bytes`, so round-trip
/// through it and let inference keep the type anonymous.
///
/// Byte order needs no thought here and that is deliberate: both ids arrive
/// as `hex::decode` of the same hex strings the witness carries
/// (`account_dapp_id_hex = hex::encode(ctx.account_dapp_id)`,
/// `bridge-event-witness/src/lib.rs:104`, decoded by `parse_hex_array::<32>`
/// at `prover.rs:191`). Feed the bytes exactly as decoded — do not reverse
/// them "to fix endianness". `bytes_to_fr` reads them little-endian and
/// `fr_to_bytes` writes them back the same way; the reduction mod p in
/// between is what the circuit does too.
pub fn withdrawal_identity_frs(dapp_id: &[u8; 32], account_id: &[u8; 32]) -> (U256, U256) {
    use gosh_dense_balanced_tree::{bytes_to_fr, fr_to_bytes};
    let as_u256 = |id: &[u8; 32]| U256::from_le_slice(&fr_to_bytes(bytes_to_fr(id)));
    (as_u256(dapp_id), as_u256(account_id))
}

/// `eth_getCode` must return something. Factored out because the message
/// is the whole value of the check: "adapter has no code" tells an operator
/// what to look at; a decode error three calls later does not.
async fn require_code<P: Provider>(provider: &P, at: Address, label: &str) -> CliResult<()> {
    let code = provider
        .get_code_at(at)
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!("{label} {at}: eth_getCode failed: {e}"),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    if code.is_empty() {
        return Err(CliError::Preflight {
            reason: format!(
                "{label} {at}: no contract code at this address — wrong address, an EOA, or an \
                 address from a different network"
            ),
            source: None,
        });
    }
    Ok(())
}

/// Preflight the destination *deploy*: something is there, its withdrawal
/// verifier stack is real and is the one this build proves against, its
/// pinned AN-side identity is ours, and its treasury visibly covers this
/// amount.
///
/// Key-free, so `--dry-run` exercises all of it.
///
/// Ordering is deliberate: cheapest and most-likely-wrong first. A typo'd
/// `--bridge-address` is far more common than a mis-deployed verifier, and
/// the operator should see the useful message, not a decode failure three
/// calls deeper.
///
/// The treasury check is a **preflight, not a promise** — the same rule the
/// ticket sets for the multisig's ECC[3] balance. Between this call and
/// `withdrawByProof` (up to ~101 min later) other withdrawals can drain the
/// treasury and operators can top it up; the chain has the last word. Its
/// job is to refuse now when it is already visible that the payout cannot
/// happen, not to guarantee that it will.
///
/// The identity and verifier checks carry no such caveat: both read
/// `immutable` storage, so what this function sees is what
/// `withdrawByProof` will see.
pub async fn check_bridge_deploy(
    rpc_url: &str,
    bridge: Address,
    // `verifiers_dir` is `None` on a dry run that did not pass
    // `--verifiers-dir` (Task 1 makes it submit-only). Everything else
    // still runs; only the deployed-bytecode comparison is skipped, and it
    // is skipped loudly. A `///` here would not compile — doc comments
    // cannot be applied to function parameters.
    verifiers_dir: Option<&Path>,
    expected_identity: (U256, U256),
    amount: &UsdcAmount,
) -> CliResult<()> {
    let url = rpc_url.parse().map_err(|e| CliError::ArgInvalid {
        flag: "rpc-url",
        expected: "an http(s) JSON-RPC URL".into(),
        got: format!("{rpc_url} ({e})"),
    })?;
    let provider = ProviderBuilder::new().connect_http(url);

    // 1. Is there a contract at all? An EOA, or an address from another network,
    //    returns empty code — the single most likely typo, and today it surfaces as
    //    a confusing decode failure in stage 4b.
    require_code(&provider, bridge, "--bridge-address").await?;
    let client = EthBridgeClient::new(bridge, provider.clone());

    // 2. The withdrawal verifier stack, all three levels. A non-zero address proves
    //    nothing: an EOA answers every call with empty returndata, and a half-wired
    //    adapter answers the first and not the second. This is the same walk
    //    `deploy/shellnet-l2/scripts/preflight.sh:28` performs.
    let adapter = client
        .withdrawal_verifier()
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "--bridge-address {bridge}: bridgeWithdrawalVerifier() failed: {e} — is this an \
                 AckiNackiBridge deploy?"
            ),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    if adapter == Address::ZERO {
        return Err(CliError::Preflight {
            reason: format!(
                "--bridge-address {bridge}: bridgeWithdrawalVerifier is the zero address — this \
                 deploy cannot verify withdrawal proofs (withdrawByProof reverts \
                 WithdrawByProofDisabled)"
            ),
            source: None,
        });
    }
    require_code(&provider, adapter, "withdrawal verifier adapter").await?;
    let (wrapper, yul) = client
        .shplonk_stack(adapter)
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "withdrawal verifier adapter {adapter}: could not walk shplonkVerifier() → \
                 yulVerifier(): {e}. The adapter is not the shape this bridge needs."
            ),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    require_code(&provider, wrapper, "withdrawal SHPLONK wrapper").await?;
    require_code(&provider, yul, "withdrawal Yul verifier").await?;

    // 3. Is the deployed Yul runtime the verifier this build proves against? A
    //    verifier from a different circuit accepts nothing we can produce, and the
    //    failure lands in stage 6 — after the burn, the anchor wait AND the proof.
    match verifiers_dir {
        None => {
            // A dry run without --verifiers-dir. Say so: silence here would
            // let an operator read a clean dry run as "the deployed verifier
            // was checked", which is the one conclusion it does not support.
            tracing::warn!(
                "no --verifiers-dir: skipping the deployed-verifier bytecode check. Pass \
                 --verifiers-dir to a dry run to exercise it — the flag is submit-only for the \
                 prover, but this check reads it directly and needs no key."
            );
        },
        Some(dir) => {
            let bin_path = dir.join(WITHDRAW_VERIFIER_BIN);
            let bin = std::fs::read(&bin_path).map_err(|e| CliError::Preflight {
                reason: format!("--verifiers-dir: read {}: {e}", bin_path.display()),
                source: None,
            })?;
            let expected = expected_verifier_runtime(&bin)?;
            let on_chain = provider
                .get_code_at(yul)
                .await
                .map_err(|e| CliError::Preflight {
                    reason: format!("eth_getCode({yul}) failed: {e}"),
                    source: Some(anyhow::anyhow!("{e}")),
                })?;
            if on_chain.as_ref() != expected {
                return Err(CliError::Preflight {
                    reason: format!(
                        "the withdrawal verifier deployed at {yul} is not {}: {} bytes on chain \
                         vs {} bytes expected. Proofs this build produces would be rejected on \
                         submit, after the burn and the ~91 min anchor wait.\n\x20 Running your \
                         own bridge deploy? Point --verifiers-dir at your own verifiers \
                         directory; see the advanced runbook's \"Running your own verifier\" \
                         section.",
                        WITHDRAW_VERIFIER_BIN,
                        on_chain.len(),
                        expected.len(),
                    ),
                    source: None,
                });
            }
        },
    }

    // 4. Identity. `withdrawByProof` compares these before it verifies anything
    //    (`AckiNackiBridge.sol:1164`), so a bridge pinned to a different AN-side
    //    account rejects every proof we can ever build. Both sides are immutable —
    //    this is a decision, not a snapshot.
    let on_chain_identity =
        client
            .withdrawal_identity()
            .await
            .map_err(|e| CliError::Preflight {
                reason: format!(
                    "--bridge-address {bridge}: reading the pinned identity failed: {e}"
                ),
                source: Some(anyhow::anyhow!("{e}")),
            })?;
    if on_chain_identity != expected_identity {
        return Err(CliError::Preflight {
            reason: format!(
                "--bridge-address {bridge} is pinned to a different Acki Nacki bridge account: on \
                 chain (dappFr, accFr) = ({}, {}), this withdrawal would prove ({}, {}). \
                 withdrawByProof reverts WithdrawIdentityMismatch before it even verifies the \
                 proof.\n\x20 Check USDC_BRIDGE_ACCOUNT_ID in $BRIDGE_CONFIG against the bridge \
                 you are withdrawing from.",
                on_chain_identity.0, on_chain_identity.1, expected_identity.0, expected_identity.1,
            ),
            source: None,
        });
    }

    // 5. Treasury preflight. See the note above: this refuses early, it does not
    //    promise.
    let treasury = client
        .treasury_balance()
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!("--bridge-address {bridge}: treasuryBalance() failed: {e}"),
            source: Some(anyhow::anyhow!("{e}")),
        })?;
    let needed = U256::from(amount.0);
    if treasury < needed {
        return Err(CliError::Preflight {
            reason: format!(
                "--bridge-address {bridge}: treasuryBalance is {treasury} µUSDC but this \
                 withdrawal needs {needed} ({}). withdrawByProof would revert \
                 WithdrawTreasuryShortfall. Top the treasury up (README Step 3) and re-run.\n\x20 \
                 Note this is a preflight, not a guarantee — the treasury is shared, and it can \
                 be drained again while this withdrawal waits for its anchor bundle.",
                amount.display(),
            ),
            source: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_accepts_0x_prefix() {
        assert_eq!(
            normalize_u256_hex("0x1234").unwrap(),
            "0000000000000000000000000000000000000000000000000000000000001234"
        );
    }

    #[test]
    fn normalize_accepts_bare_hex() {
        let a = "abababababababababababababababababababababababababababababababab";
        assert_eq!(normalize_u256_hex(a).unwrap(), a);
    }

    #[test]
    fn normalize_accepts_decimal() {
        assert_eq!(
            normalize_u256_hex("255").unwrap(),
            "00000000000000000000000000000000000000000000000000000000000000ff"
        );
    }

    #[test]
    fn normalize_rejects_junk() {
        assert!(normalize_u256_hex("not-a-number").is_none());
        assert!(normalize_u256_hex("").is_none());
    }

    #[test]
    fn pubkeys_equal_is_case_insensitive() {
        assert!(pubkeys_equal("ABcd", "abcd"));
        assert!(!pubkeys_equal("ab", "cd"));
    }

    #[test]
    fn parse_custodians_ok_single() {
        let j = json!({
            "custodians": [
                { "owner_pubkey": "0x1234", "owner_address": null, "index": 0 }
            ]
        });
        let out = parse_custodians(&j).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].owner_pubkey_hex.as_deref().unwrap(),
            "0000000000000000000000000000000000000000000000000000000000001234"
        );
    }

    #[test]
    fn parse_custodians_ok_empty() {
        let j = json!({ "custodians": [] });
        assert_eq!(parse_custodians(&j).unwrap().len(), 0);
    }

    #[test]
    fn parse_custodians_ok_null_pubkey() {
        let j = json!({
            "custodians": [
                { "owner_pubkey": null, "owner_address": "0:...", "index": 0 }
            ]
        });
        let out = parse_custodians(&j).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].owner_pubkey_hex.is_none());
    }

    #[test]
    fn parse_custodians_rejects_missing_array() {
        assert!(parse_custodians(&json!({})).is_err());
    }

    #[test]
    fn describe_account_status_covers_all_variants() {
        assert_eq!(describe_account_status(AccountStatus::AccStateActive), "Active");
        assert_eq!(describe_account_status(AccountStatus::AccStateUninit), "Uninit");
        assert_eq!(describe_account_status(AccountStatus::AccStateFrozen), "Frozen");
        assert_eq!(describe_account_status(AccountStatus::AccStateNonexist), "NonExist");
    }

    #[test]
    fn eth_private_key_is_rejected_before_any_network_call() {
        // A malformed key must be caught by shape alone — no RPC needed. The
        // URL below is deliberately unroutable: if this test ever hangs or
        // fails on connectivity, the parse order regressed.
        let err =
            parse_eth_signer("not-a-key").expect_err("a malformed burner key must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("--eth-private-key"),
            "refusal must name the flag, got: {msg}"
        );
        assert!(
            !msg.contains("not-a-key"),
            "refusal must not echo key material, got: {msg}"
        );
        // The leak that a whole-string check misses: hex decoders name the bad
        // character and its offset, which is a byte of the key.
        for ch in "not-a-key".chars().filter(|c| !c.is_whitespace()) {
            assert!(
                !msg.contains(&format!("'{ch}'")),
                "refusal must not quote any character of the input, got: {msg}",
            );
        }
        assert!(
            msg.contains("<redacted"),
            "refusal must say only how long it was, got: {msg}",
        );
    }

    #[test]
    fn balance_check_variants_are_not_interchangeable() {
        // Guards the one way this can silently break: `from_burn_sent` wired
        // backwards would disable the sufficiency check on every fresh run
        // and enforce it on every resume — both failures are invisible until
        // money is involved.
        assert!(matches!(
            BalanceCheck::from_burn_sent(false),
            BalanceCheck::Require
        ));
        assert!(matches!(
            BalanceCheck::from_burn_sent(true),
            BalanceCheck::SkipSpent
        ));
    }

    #[test]
    fn eth_signer_accepts_a_well_formed_key() {
        let k = "0x0000000000000000000000000000000000000000000000000000000000000001";
        parse_eth_signer(k).expect("a well-formed key must be accepted");
    }

    // -- EVM-side preflight ---------------------------------------------

    #[test]
    fn verifier_runtime_strips_the_create_prelude() {
        // gen_evm_verifier_shplonk emits a 32-byte CREATE prelude followed by
        // the runtime payload; eth_getCode returns only the payload. Off-by-one
        // here makes every comparison fail and the flag look broken.
        let mut bin = vec![0xAAu8; 32];
        bin.extend_from_slice(&[0x60, 0x80, 0x60, 0x40]);
        assert_eq!(expected_verifier_runtime(&bin).unwrap(), &[
            0x60, 0x80, 0x60, 0x40
        ]);

        let too_short = vec![0u8; 32];
        expected_verifier_runtime(&too_short)
            .expect_err("a bin with no payload after the prelude is not a verifier");
    }

    #[test]
    fn identity_frs_match_the_prover() {
        // Same conversion the prover uses for public inputs [6] and [7]
        // (`bridge-event-prover-lib/src/prover.rs:232-233`). If this drifts,
        // the CLI cheerfully approves a bridge that will revert
        // WithdrawIdentityMismatch on every proof it ever submits.
        let dapp = [0x11u8; 32];
        let acc = [0x22u8; 32];
        let (dapp_fr, acc_fr) = withdrawal_identity_frs(&dapp, &acc);
        let expect = |id: &[u8; 32]| {
            U256::from_le_slice(&gosh_dense_balanced_tree::fr_to_bytes(
                gosh_dense_balanced_tree::bytes_to_fr(id),
            ))
        };
        assert_eq!(dapp_fr, expect(&dapp));
        assert_eq!(acc_fr, expect(&acc));
        assert_ne!(dapp_fr, acc_fr, "the two halves must not collapse");

        // The reduction is not decorative: an id at or above the BN254
        // modulus wraps, and the contract stores the wrapped value. A helper
        // that skipped Fr and did `U256::from_le_bytes(id)` would agree on
        // small ids and diverge exactly where it matters.
        let big = [0xFFu8; 32];
        assert_ne!(
            withdrawal_identity_frs(&big, &big).0,
            U256::from_le_slice(&big),
            "the id must be reduced mod p, not reinterpreted"
        );
    }

    /// Function selectors, pinned with
    /// `cast sig 'treasuryBalance()'` &c. The mock answers `0x` to anything
    /// it does not recognise, so a stale selector here makes a test fail
    /// loudly on an empty decode rather than silently pass.
    const SEL_TREASURY: &str = "313dab20"; // treasuryBalance()
    const SEL_VERIFIER: &str = "1792b9fe"; // bridgeWithdrawalVerifier()
    const SEL_DAPP_FR: &str = "e20b7f65"; // bridgeWithdrawalDappFr()
    const SEL_ACC_FR: &str = "5c987786"; // bridgeWithdrawalAccFr()
    const SEL_SHPLONK: &str = "66dbcfb5"; // shplonkVerifier()
    const SEL_YUL: &str = "c74e1862"; // yulVerifier()

    /// A 32-byte zero word — what a getter returns when its slot is unset.
    const ZERO_WORD: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
    /// A treasury so large that no test in this group trips the shortfall
    /// branch by accident.
    const MAX_WORD: &str = "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    /// Any non-empty runtime. `require_code` only asks whether there is
    /// code, so the bytes need not be a real contract.
    const SOME_CODE: &str = "0x60806040";

    /// `Address::repeat_byte(n)` as a left-padded 32-byte ABI word — the
    /// form an `address` getter returns.
    fn word_addr(n: u8) -> String {
        format!("0x{}{}", "00".repeat(12), format!("{n:02x}").repeat(20))
    }

    /// A one-shot JSON-RPC responder. `answers` maps a 4-byte selector
    /// (hex, no `0x`) to the word to return; `eth_getCode` is answered from
    /// `code` for every address. Anything unmapped returns `0x`, which is
    /// what a wrong address looks like on a real node.
    async fn mock_rpc(
        code: &'static str,
        answers: std::collections::HashMap<&'static str, String>,
    ) -> String {
        mock_rpc_inner(CodeAnswer::Everywhere(code), answers).await
    }

    /// Per-address `eth_getCode`. The case a single shared answer cannot
    /// express — bridge has code, adapter does not — and the case that
    /// catches a wired-up EOA.
    async fn mock_rpc_code_for(
        code: &[(Address, &'static str)],
        answers: std::collections::HashMap<&'static str, String>,
    ) -> String {
        let map = code
            .iter()
            .map(|(a, c)| (a.to_string().to_lowercase(), *c))
            .collect();
        mock_rpc_inner(CodeAnswer::PerAddress(map), answers).await
    }

    enum CodeAnswer {
        Everywhere(&'static str),
        PerAddress(std::collections::HashMap<String, &'static str>),
    }

    impl CodeAnswer {
        fn for_address(&self, addr: &str) -> String {
            match self {
                CodeAnswer::Everywhere(c) => (*c).to_string(),
                CodeAnswer::PerAddress(m) => m
                    .get(&addr.to_lowercase())
                    .copied()
                    .unwrap_or("0x")
                    .to_string(),
            }
        }
    }

    async fn mock_rpc_inner(
        code: CodeAnswer,
        answers: std::collections::HashMap<&'static str, String>,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let code = std::sync::Arc::new(code);
        let answers = std::sync::Arc::new(answers);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let answers = answers.clone();
                let code = code.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let body = req.rsplit("\r\n\r\n").next().unwrap_or("").to_string();
                    let v: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                    let id = v.get("id").cloned().unwrap_or(serde_json::json!(1));
                    let result = match v.get("method").and_then(|m| m.as_str()) {
                        Some("eth_chainId") => "0xaa36a7".to_string(), // 11155111
                        Some("eth_getCode") => {
                            let at = v["params"][0].as_str().unwrap_or("");
                            code.for_address(at)
                        },
                        Some("eth_call") => {
                            // `input` first. Alloy 2 builds contract calls with
                            // `with_input` (`alloy-contract-2.4.1/src/call.rs:474`),
                            // which sets `TransactionInput::input`
                            // (`alloy-network-2.4.1/src/ethereum/builder.rs:34`)
                            // and leaves `data` unset — so the request carries
                            // `"input": "0x…"` and no `"data"` at all. Reading
                            // `data` yields "", the selector is empty, every
                            // lookup misses, the mock answers `0x`, and every
                            // test here passes or fails for reasons unrelated
                            // to what it claims to check.
                            //
                            // `data` stays as a fallback: it is still valid
                            // JSON-RPC, other clients send it, and the cost is
                            // one `or_else`.
                            let call = &v["params"][0];
                            let data = call["input"]
                                .as_str()
                                .or_else(|| call["data"].as_str())
                                .unwrap_or("");
                            let sel = data.trim_start_matches("0x").get(..8).unwrap_or("");
                            answers
                                .get(sel)
                                .cloned()
                                .unwrap_or_else(|| "0x".to_string())
                        },
                        _ => "0x".to_string(),
                    };
                    let payload =
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
                         {}\r\n\r\n{}",
                        payload.len(),
                        payload
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn an_address_with_no_code_is_refused() {
        let url = mock_rpc("0x", Default::default()).await;
        let err = check_bridge_deploy(
            &url,
            Address::ZERO,
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("an EOA or a wrong-network address must be refused");
        assert!(format!("{err}").contains("no contract code"), "got: {err}");
    }

    #[tokio::test]
    async fn a_zero_withdrawal_verifier_is_refused() {
        let answers = std::collections::HashMap::from([(SEL_VERIFIER, ZERO_WORD.to_string())]);
        let url = mock_rpc(SOME_CODE, answers).await;
        let err = check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("withdrawByProof reverts WithdrawByProofDisabled on a zero verifier");
        assert!(format!("{err}").contains("cannot verify"), "got: {err}");
    }

    #[tokio::test]
    async fn a_verifier_adapter_with_no_code_is_refused() {
        // The case a non-zero check cannot see: the address is set, and it is
        // an EOA. `eth_getCode` is answered "0x" for every other address by
        // this mock, so the bridge itself must carry code — served separately.
        let answers = std::collections::HashMap::from([(SEL_VERIFIER, word_addr(2))]);
        let url = mock_rpc_code_for(&[(Address::repeat_byte(1), SOME_CODE)], answers).await;
        let err = check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("a verifier adapter that is an EOA must be refused");
        assert!(format!("{err}").contains("adapter"), "got: {err}");
    }

    #[tokio::test]
    async fn an_identity_mismatch_is_refused_before_the_burn() {
        // The identity check is the LAST of the four, so the mock has to get
        // the run all the way through the verifier walk first: adapter (2) →
        // wrapper (3) → yul (4), each answering its getter and each carrying
        // code. A mock that stops at the adapter fails on an empty `eth_call`
        // return long before the assertion, and the test would be green for
        // the wrong reason — or red for one.
        let answers = std::collections::HashMap::from([
            (SEL_VERIFIER, word_addr(2)),         // bridge → adapter
            (SEL_SHPLONK, word_addr(3)),          // adapter → wrapper
            (SEL_YUL, word_addr(4)),              // wrapper → yul
            (SEL_DAPP_FR, ZERO_WORD.to_string()), // on chain: 0
            (SEL_ACC_FR, ZERO_WORD.to_string()),
            (SEL_TREASURY, MAX_WORD.to_string()), // not what this test is about
        ]);
        let url = mock_rpc_code_for(
            &[
                (Address::repeat_byte(1), SOME_CODE),
                (Address::repeat_byte(2), SOME_CODE),
                (Address::repeat_byte(3), SOME_CODE),
                (Address::repeat_byte(4), SOME_CODE),
            ],
            answers,
        )
        .await;
        let err = check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::from(7u8), U256::from(9u8)), // what our proof will carry
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("WithdrawIdentityMismatch is checked before proof verification");
        let msg = format!("{err}");
        assert!(
            msg.contains("WithdrawIdentityMismatch"),
            "must name the revert, got: {msg}"
        );
        assert!(
            msg.contains("different Acki Nacki"),
            "must say what it means, got: {msg}"
        );
    }

    fn sample_from() -> FromAddress {
        FromAddress {
            dapp_id_hex: "a".repeat(64),
            account_id_hex: "b".repeat(64),
        }
    }

    fn sample_to() -> ToAddress {
        ToAddress {
            address: "0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f"
                .parse()
                .unwrap(),
            chain_id: 11_155_111,
        }
    }

    /// The two id byte arrays and the two display strings describe the
    /// same values — that is exactly what the test below asserts, so
    /// building them from one pair of variables is the point.
    fn sample_report() -> PreflightReport {
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
            // A literal, deliberately. The pinned real key pair
            // (`PAIR_PUBLIC`) arrives in Task 3, and this task ends with a
            // green `cargo test` — reaching forward for it would leave
            // Task 2 uncompilable in a sequential run. Nothing here reads
            // this field: the test below asserts on the id fields only.
            owner_pubkey_hex: "ee".repeat(32),
        }
    }

    #[test]
    fn the_report_ids_agree_with_the_strings_they_came_from() {
        let r = sample_report();
        assert_eq!(
            format!(
                "{}::{}",
                hex::encode(r.bridge_dapp_id),
                hex::encode(r.bridge_account_id)
            ),
            r.usdc_bridge_extended,
            "the typed ids and the display string must be the same two values"
        );
    }

    // -- --from-keys ------------------------------------------------------

    // The pinned pair is declared once, in `crate::test_keys` — do NOT
    // redeclare it here. Two copies that drift produce a keys.json whose
    // halves do not match, and that fails inside the SDK rather than
    // visibly.
    use crate::test_keys::{PAIR_PUBLIC, PAIR_SECRET};

    /// Write a keys.json and return its path.
    fn write_keys(dir: &std::path::Path, public: &str, secret: &str) -> std::path::PathBuf {
        let path = dir.join("keys.json");
        std::fs::write(
            &path,
            format!(r#"{{"public":"{public}","secret":"{secret}"}}"#),
        )
        .unwrap();
        path
    }

    #[test]
    fn keypair_missing_secret_is_a_preflight_refusal() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("keys.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"{{"public":"{}"}}"#, "a".repeat(64)).unwrap();

        let err = load_owner_keypair_hex(&path)
            .expect_err("a keys.json with no secret half must be refused at preflight");
        let msg = format!("{err}");
        assert!(msg.contains("--from-keys"), "must name the flag, got: {msg}");
        assert!(
            msg.contains("secret"),
            "must name the missing field, got: {msg}"
        );
    }

    #[test]
    fn keypair_with_matching_halves_is_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_keys(dir.path(), PAIR_PUBLIC, PAIR_SECRET);

        let (public, secret) = load_owner_keypair_hex(&path).expect("a real pair must be accepted");
        assert_eq!(public, PAIR_PUBLIC);
        assert_eq!(secret, PAIR_SECRET);
    }

    #[test]
    fn keypair_with_mismatched_halves_is_refused() {
        // The whole point of the ticket's "валидная пара" wording. Two
        // independently well-formed 64-hex strings are NOT a key pair, and
        // letting them through means the multisig rejects the signature after
        // the message is already on the wire — an ambiguous exit 10 instead of
        // a clean exit 2.
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_keys(dir.path(), &"a".repeat(64), &"1".repeat(64));

        let err = load_owner_keypair_hex(&path)
            .expect_err("independent public/secret halves must be refused");
        let msg = format!("{err}");
        assert!(msg.contains("--from-keys"), "must name the flag, got: {msg}");
        assert!(
            msg.contains("do not form a key pair") || msg.contains("does not match"),
            "must say why, got: {msg}",
        );
        assert!(
            !msg.contains(&"a".repeat(64)) && !msg.contains(&"1".repeat(64)),
            "must not echo any half of the key file, got: {msg}",
        );
    }
}
