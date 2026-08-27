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

use bridge_gql_fetcher::gql_client::{create_client, GqlClient};
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
    pub multisig_ecc3_balance: u128,
    pub owner_pubkey_hex: String,
}

/// Run every preflight check in order and return the aggregated report.
///
/// Sequence:
/// 1. `--from-keys` file permissions (already checked by args::validate,
///    re-checked here so `preflight` is self-contained for callers that
///    skip arg validation).
/// 2. `--from` account exists on AN, is `Active`, has non-zero code hash.
/// 3. `--from` is a multisig (typed `getCustodians` call succeeds).
/// 4. `custodianCount == 1` (single-custodian invariant; multi-owner
///    sends would need submit+confirm from other owners → contract
///    exit 108).
/// 5. Owner pubkey decoded from `--from-keys` matches multisig's
///    on-chain owner pubkey.
/// 6. USDCBridge live dapp_id resolved via GraphQL (canonical account
///    default overridden by `--usdc-bridge-account`).
/// 7. Multisig ECC[3] balance ≥ amount (preflight, not guarantee — the
///    chain has the final word between check and send).
pub async fn run(
    from: &FromAddress,
    from_keys: &Path,
    to: &ToAddress,
    amount: &UsdcAmount,
    gql_endpoint: &str,
    usdc_bridge_account_id_hex: &str,
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
    let custodians = call_get_custodians(&context, &from.extended(), &account_boc).await?;
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
    let local_pubkey = load_owner_pubkey_hex(from_keys)?;
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

    // 7. ECC[3] balance from the parsed account.
    let ecc3_balance = extract_ecc_balance(&account, 3)?;
    if ecc3_balance < amount.0 {
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
    }

    Ok(PreflightReport {
        from: from.clone(),
        to: *to,
        amount: *amount,
        usdc_bridge_extended,
        usdc_bridge_legacy,
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
    let q = format!(
        r#"{{ blockchain {{ account(account_id: "{account_id_hex}", dapp_id: "{zero64}") {{ info {{ dapp_id acc_type }} }} }} }}"#
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
        .get("acc_type")
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

/// Load the ed25519 public half from a TVM keys.json. The secret half is
/// touched only insofar as `read_to_string` reads the whole file; it never
/// leaves this function. The returned public hex is 64 lowercase chars.
fn load_owner_pubkey_hex(path: &Path) -> CliResult<String> {
    let contents = std::fs::read_to_string(path).map_err(|_| CliError::KeyFilePerms {
        path: path.display().to_string(),
    })?;
    let json: Value = serde_json::from_str(&contents).map_err(|_| CliError::Preflight {
        reason: format!("--from-keys {}: not valid JSON", path.display()),
        source: None,
    })?;
    let pubkey = json.get("public").and_then(|v| v.as_str()).ok_or_else(|| {
        CliError::Preflight {
            reason: format!(
                "--from-keys {}: missing 'public' string field",
                path.display()
            ),
            source: None,
        }
    })?;
    normalize_u256_hex(pubkey).ok_or_else(|| CliError::Preflight {
        reason: format!(
            "--from-keys {}: 'public' field is not a valid uint256",
            path.display()
        ),
        source: None,
    })
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

fn pubkeys_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
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
}
