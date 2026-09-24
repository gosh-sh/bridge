//! Preflight checks. No signing, no sends — nothing here can move money or
//! reach either chain's mempool.
//!
//! Not side-effect free, though: `check_output_dirs` creates the prover's
//! output directories and writes a short-lived probe file into each, on the
//! principle that a path we cannot write at stage 1 is a crash at stage 5,
//! after the burn. Nothing else here writes.
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

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy::{
    primitives::{Address, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use bridge_gql_fetcher::gql_client::{create_client, GqlClient};
use bridge_prover_lib::keys::{leaked_keygen_temp_files, probe_ceremony, KeyCacheState};
use bridge_relayer_daemon::bridge::EthBridgeClient;
use serde_json::{json, Value};
use tvm_block::{Account, AccountStatus, Deserializable};
use tvm_client::{
    abi::{encode_message, Abi, CallSet, ParamsOfEncodeMessage, Signer},
    account::{get_account, ParamsOfGetAccount},
    net::NetworkConfig,
    tvm::{run_tvm, ParamsOfRunTvm},
    ClientConfig, ClientContext,
};

use crate::{
    args::{self, FromAddress, ToAddress, UsdcAmount},
    errors::{CliError, CliResult},
};

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
    /// The balance rule for a run whose burn is already on the
    /// wire: a resume must not re-check ECC[3], because the burn
    /// it is resuming already spent it.
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
/// 1. `--from-keys` file permissions. THE check, not a second one:
///    `args::check_key_file_perms` has exactly one production caller and it is
///    the line below. This list used to say "already checked by
///    args::validate", and `args::validate` does not exist — so the one thing
///    standing between a world-readable secret key and a signing run was
///    described as a belt-and-braces repeat of something that never ran, which
///    is how a check gets deleted as redundant.
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
    // 1. File perms. Nothing ran this before us; see the list above.
    args::check_key_file_perms(from_keys)?;

    // Shared tvm-sdk client — used for account fetch + local getter exec.
    let context = build_client_context(gql_endpoint)?;

    // 2. Fetch --from account BOC, parse via tvm_block::Account, verify Active +
    //    has code_hash.
    let account = fetch_account(&context, &from.dapp_id_hex, &from.account_id_hex).await?;
    let account_boc = fetch_account_boc(&context, &from.dapp_id_hex, &from.account_id_hex).await?;
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
                "--from account {}: no code_hash (Active but codeless — impossible without \
                 corruption)",
                from.extended()
            ),
            source: None,
        });
    }

    // 3+4+5. run_tvm(getCustodians) → parse → count == 1 → pubkey match.
    // Encode+run_tvm needs the legacy `0:<acc>` form — tvm-sdk v3.0.6.an's
    // ABI message encoder rejects the v3 `dapp_id::acc_id` shape here (works
    // for `get_account` above and for tvm-cli `--addr`, but not for the
    // `address` field on `ParamsOfEncodeMessage`). See memory
    // `tvm_cli_v3_address_forms.md`.
    let custodians = call_get_custodians(&context, &from.legacy(), &account_boc).await?;
    if custodians.len() != 1 {
        return Err(CliError::Preflight {
            reason: format!(
                "--from account {}: single-custodian multisig required, found {} custodians. This \
                 CLI's sendTransaction path signs alone and cannot satisfy multi-owner \
                 confirmation (would fail with contract exit 108 on-chain).",
                from.extended(),
                custodians.len()
            ),
            source: None,
        });
    }
    let on_chain_pubkey =
        custodians[0]
            .owner_pubkey_hex
            .as_deref()
            .ok_or_else(|| CliError::Preflight {
                reason: format!(
                    "--from account {}: custodian 0 has no owner_pubkey — this CLI does not \
                     support address-owned custodians (only pubkey-owned).",
                    from.extended()
                ),
                source: None,
            })?;
    let (local_pubkey, _secret) = load_owner_keypair_hex(from_keys)?;
    if !pubkeys_equal(&local_pubkey, on_chain_pubkey) {
        // Do NOT echo the local key back — even the public half is a stable
        // identifier that a user might not want in shell history. The
        // on-chain half is public; showing that alone is enough to guide
        // "you gave me the wrong keys.json".
        return Err(CliError::Preflight {
            reason: format!(
                "--from-keys does not match on-chain owner of {}: expected pubkey {} (from \
                 getCustodians)",
                from.extended(),
                on_chain_pubkey
            ),
            source: None,
        });
    }

    // 6. USDCBridge lookup: resolve dapp_id AND verify the account is Active. One
    //    GQL round-trip through the same helper the daemon uses.
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

/// The one place a `tvm_client` context is constructed.
///
/// One place because the failure's exit code is a decision, and a second
/// copy made it twice: this one refuses with exit 2, the orchestrator's
/// copy reported exit 10 — published as "the AN burn WAS broadcast" — for
/// a construction that happens before anything is composed, let alone
/// sent. `ClientContext::new` only builds config and does not connect, so
/// every one of its failures is local and pre-send.
pub(crate) fn build_client_context(gql_endpoint: &str) -> CliResult<Arc<ClientContext>> {
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
        source: Some(anyhow::Error::new(e)),
    })?;
    Ok(Arc::new(ctx))
}

// -- Account fetch + parse (checks 2 + 7) ------------------------------------

/// The `--from` account's BOC, as the node serves it.
async fn fetch_account_boc(
    ctx: &Arc<ClientContext>,
    dapp_id_hex: &str,
    account_id_hex: &str,
) -> CliResult<String> {
    let params = ParamsOfGetAccount {
        account_id: account_id_hex.to_string(),
        dapp_id: dapp_id_hex.to_string(),
    };
    let r = get_account(ctx.clone(), params)
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!(
                "fetch account {dapp_id_hex}::{account_id_hex}: {}",
                e.message()
            ),
            source: Some(anyhow::anyhow!("{e:?}")),
        })?;
    if r.boc.is_empty() {
        return Err(CliError::Preflight {
            reason: format!("account {dapp_id_hex}::{account_id_hex}: not on chain (empty BOC)"),
            source: None,
        });
    }
    Ok(r.boc)
}

/// The `--from` account, parsed from the BOC the node serves.
async fn fetch_account(
    ctx: &Arc<ClientContext>,
    dapp_id_hex: &str,
    account_id_hex: &str,
) -> CliResult<Account> {
    let boc = fetch_account_boc(ctx, dapp_id_hex, account_id_hex).await?;
    Account::construct_from_base64(&boc).map_err(|e| CliError::Preflight {
        reason: format!("parse account BOC for {dapp_id_hex}::{account_id_hex}: {e}"),
        source: Some(e),
    })
}

/// An account status in the words the node's own tooling uses, so
/// a refusal and a `tvm-cli` session agree.
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

/// Run `getCustodians` on the account's own code, locally. This is
/// the check that decides whether `--from-keys` matches the
/// on-chain owner.
async fn call_get_custodians(
    ctx: &Arc<ClientContext>,
    address_extended: &str,
    account_boc: &str,
) -> CliResult<Vec<Custodian>> {
    let abi = Abi::Json(MULTISIG_ABI_JSON.to_string());

    // Encode an empty-input getCustodians call as an unsigned external
    // message aimed at the multisig. This is the same shape tvm-cli's
    // `runx` builds under the hood.
    let encoded = encode_message(ctx.clone(), ParamsOfEncodeMessage {
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
    })
    .await
    .map_err(|e| CliError::Preflight {
        reason: format!(
            "encode getCustodians call for {address_extended}: {}",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;

    let run = run_tvm(ctx.clone(), ParamsOfRunTvm {
        message: encoded.message,
        account: account_boc.to_string(),
        abi: Some(abi),
        execution_options: None,
        boc_cache: None,
        return_updated_account: Some(false),
    })
    .await
    .map_err(|e| CliError::Preflight {
        reason: format!(
            "run_tvm getCustodians on {address_extended}: {} — is this address a deployed \
             multisig?",
            e.message()
        ),
        source: Some(anyhow::anyhow!("{e:?}")),
    })?;

    let decoded = run
        .decoded
        .and_then(|d| d.output)
        .ok_or_else(|| CliError::Preflight {
            reason: format!(
                "run_tvm getCustodians on {address_extended}: no decoded output (ABI mismatch?)"
            ),
            source: None,
        })?;
    parse_custodians(&decoded)
}

/// The custodian list out of a `getCustodians` answer, refusing
/// anything it cannot read rather than reporting zero owners.
fn parse_custodians(json: &Value) -> CliResult<Vec<Custodian>> {
    let arr = json
        .get("custodians")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CliError::Preflight {
            reason: format!("getCustodians output missing `custodians` array: {json}"),
            source: None,
        })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let owner_pubkey_hex = match entry.get("owner_pubkey") {
            Some(Value::Null) | None => None,
            Some(v) => {
                let s = v.as_str().ok_or_else(|| CliError::Preflight {
                    reason: format!("getCustodians[{i}].owner_pubkey: expected string, got {v}"),
                    source: None,
                })?;
                Some(normalize_u256_hex(s).ok_or_else(|| CliError::Preflight {
                    reason: format!("getCustodians[{i}].owner_pubkey: not a valid uint256: {s}"),
                    source: None,
                })?)
            },
        };
        out.push(Custodian {
            owner_pubkey_hex,
        });
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
                "USDCBridge account {account_id_hex}: GraphQL returned no account node — wrong \
                 --usdc-bridge-account, or wrong --gql-endpoint?"
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
    let dapp_id = if raw_dapp.is_empty() {
        zero64
    } else {
        raw_dapp
    };
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
        source: Some(e),
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
        // The key-file parser, not the uint256 one: see
        // `normalize_key_file_hex`. A key of all decimal digits must stay
        // hex, and `burn::load_keypair` reads the same file with the same
        // rule so the two cannot disagree.
        normalize_key_file_hex(raw).ok_or_else(|| CliError::Preflight {
            reason: format!(
                "--from-keys {}: '{field}' field is not 1-64 hex characters (an optional `0x` \
                 prefix is accepted)",
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
pub(crate) fn normalize_u256_hex(raw: &str) -> Option<String> {
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

/// Compare two ed25519 public keys.
///
/// A plain case-insensitive comparison, and that is only correct because
/// **both sides are canonicalised before they get here** — 64 lowercase hex
/// characters, no prefix, zero-padded:
///
/// * the on-chain half through [`normalize_u256_hex`], which must accept the
///   decimal form because that is one of the shapes tvm renders a `uint256` in;
/// * the key-file half through [`normalize_key_file_hex`], which must **not**,
///   because a keys.json field is always hex and a key that happens to be all
///   decimal digits would otherwise be silently reinterpreted as a decimal
///   number.
///
/// Normalising again here would undo that: applying `normalize_u256_hex` to
/// an already-canonical all-digit key turns it into a different value. The
/// asymmetry between the two parsers is the point, not an oversight.
pub(crate) fn pubkeys_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Canonicalise a hex field read out of a `keys.json`: 64 lowercase hex
/// characters, no `0x`, zero-padded on the left.
///
/// Deliberately **not** [`normalize_u256_hex`]. That one accepts a decimal
/// form, which is right for a `uint256` coming back from the chain and
/// wrong for a key file: `"1111…1111"` is a perfectly ordinary 64-character
/// hex key, and reading it as decimal yields a different 256-bit value.
/// Preflight and `burn::load_keypair` both go through here so the value
/// they compare — and the value handed to the SDK — are the same bytes.
pub(crate) fn normalize_key_file_hex(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let bare = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    if bare.is_empty() || bare.len() > 64 || !bare.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{:0>64}", bare.to_ascii_lowercase()))
}
// -- Prover artifacts --------------------------------------------------------

/// Every ceremony degree a withdrawal actually loads.
///
/// **Not a minimum — a set.** `KeyManager::new` builds all four managers,
/// and each asks `load_srs` for one exact degree:
///
/// | manager  | circuit `k` | `srs_k` loaded | source |
/// |----------|-------------|----------------|--------|
/// | primary  | 20          | **20**         | `primary.rs:44,56` |
/// | fallback | 21          | **21**         | `fallback.rs:51,62` |
/// | layer    | 17          | **20**         | `layer.rs:53,61` (`KEYGEN_SRS_K.max(k)`) |
/// | event    | 19          | **20**         | `event.rs:46,53` (`KEYGEN_SRS_K.max(k)`) |
///
/// So {20, 21}, and checking only 21 is not enough. `load_srs` tries the
/// **exact** path `kzg_bn254_{k}.srs` first and, if the header matches,
/// refuses a non-Hermez `s_g2`. A structurally valid but non-Hermez
/// `kzg_bn254_20.srs` sitting next to a perfectly good K=21 file therefore
/// passes a K=21-only probe and fails at proof time — in stage 5, after the
/// burn. The K=21 file does not rescue it, because the exact path wins
/// before any downsizing is considered.
pub const REQUIRED_CEREMONY_KS: [u32; 2] = [20, 21];

/// Bytes the first Circuit-4 keygen writes into `params_dir`
/// (`event_pk.bin` ~2.65 GB per `TECHNICAL_README.md`, plus vk and config),
/// rounded up for slack.
const EVENT_KEYGEN_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Bytes the outer SHPLONK aggregator keygen writes into the pk cache.
///
/// Measured, not guessed: `aggregator_cache.rs:16` documents the slot as
/// "proving key (SDK's `RawBytes` format, ~800 MB @ K=21)". Rounded up for
/// the `.meta.json` sibling and slack.
const OUTER_PK_BYTES: u64 = 1024 * 1024 * 1024;

/// How long the `--help` probe waits in production.
pub(crate) const AGGREGATOR_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The verifier bytecode this build expects, embedded at compile time.
///
/// A size range is not good enough: any plausibly-sized file would pass, so
/// a stale or corrupted verifier survives preflight and is caught by
/// `aggregate-proof` only at stage 5 — after the burn and after minutes of
/// proof generation. The version-control argument for accepting that is
/// wrong too: a VCS guarantees the integrity of its objects, not of a
/// working tree anything can overwrite after checkout.
///
/// Embedding the bytes makes the expectation exact and self-updating: a
/// rotated verifier is picked up by the next build, and there is no second
/// copy of a hash to drift out of sync.
const EXPECTED_VERIFIER_BIN: &[u8] =
    include_bytes!("../../../contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin");

/// The verifier source this build expects, embedded for the same reason as
/// [`EXPECTED_VERIFIER_BIN`]. `aggregate-proof` compares the source it
/// regenerates from the aggregator key with the copy in `--verifiers-dir` and
/// refuses on any difference — at stage 5, after the burn.
const EXPECTED_VERIFIER_SOL: &[u8] =
    include_bytes!("../../../contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol");

/// Fail fast when the artifacts a real run needs are absent or unusable.
///
/// Everything here is checked at stage 1 because the alternative is stage 5:
/// after the irreversible AN burn and up to ~91 min of anchor wait.
///
/// Deliberately a thin sequence over five independently callable checks.
/// They are separate functions because they must be separately testable — a
/// single fused function short-circuits on the ceremony, and every test for
/// the later checks would need a real 256 MB SRS on disk to reach its own
/// assertion.
pub async fn check_prover_artifacts(
    p: &crate::args::SubmitPlumbing,
    snark_dir: &Path,
    pk_cache_dir: Option<&Path>,
    allow_verifier_drift: bool,
) -> CliResult<Option<PkFingerprint>> {
    check_ceremony(&p.params_dir)?;
    check_verifier_bin(&p.verifiers_dir, allow_verifier_drift)?;
    check_verifier_sol(&p.verifiers_dir, allow_verifier_drift)?;
    check_aggregator_runnable(&p.aggregator_dir).await?;

    // Ask what the key cache is BEFORE deciding what must be writable. A
    // warm `params_dir` is only ever read, so demanding write access to it
    // would refuse a perfectly good read-only or shared params mount — a
    // setup the config file explicitly contemplates ("shared with the bundle
    // daemon"). Only a cold cache writes there.
    //
    // The one write a warm run might still attempt is `load_srs` persisting
    // a downsized SRS, and that is already best-effort: it warns and
    // continues on failure. Refusing the run for it would be stricter than
    // the code it is protecting.
    let cache = bridge_prover_lib::keys::probe_event_key_cache(&p.params_dir);

    // Only a `Warm` verdict means the proving key's bytes were actually
    // streamed and matched. Recording a digest for a cold or corrupt cache
    // would pin a file nothing vouched for, and stage 5 would then defend
    // a key that was never verified.
    let fingerprint = matches!(cache, KeyCacheState::Warm)
        .then(|| bridge_prover_lib::keys::event_pk_recorded_digest(&p.params_dir))
        .flatten()
        .map(|(path, sha256)| PkFingerprint {
            path,
            sha256,
        });

    // `Cold`, specifically — not `!Warm`. `Corrupt` and `Blocked` are also
    // not-warm, and on a read-only `params/` the writability probe would
    // fire first and report a permissions problem, burying the diagnosis the
    // operator actually needs ("the proving key is truncated; run --repair",
    // or "there is a directory where a key file goes"). Both are refused a
    // few lines below with those messages; neither needs a second, worse one
    // here.
    let params_needs_write = matches!(cache, KeyCacheState::Cold { .. });

    // Resolve the pk cache the way the RUNTIME resolves it, once, before
    // anything branches on it. `driver.rs:349-352` does
    // `pk_cache_dir.unwrap_or_else(|| params_dir.join("pk_cache"))`, so
    // `None` does not mean "no cache" — it means "<params_dir>/pk_cache".
    //
    // Threading the raw `Option` through instead is how the default path
    // would end up unchecked: with a warm cache `params_dir` is not passed
    // to `check_output_dirs` either, so nothing would probe the directory
    // the aggregator is about to write ~800 MB into.
    let effective_pk_cache = pk_cache_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| p.params_dir.join("pk_cache"));

    check_output_dirs(
        &p.work_dir,
        snark_dir,
        &effective_pk_cache,
        params_needs_write.then_some(p.params_dir.as_path()),
    )?;
    check_disk_headroom(&p.params_dir, &effective_pk_cache, cache)?;

    Ok(fingerprint)
}

/// The proving key as stage 1 verified it, carried to stage 5.
///
/// **Its digest is streamed exactly once before the burn.**
/// `probe_event_key_cache` hashes ~2.65 GB to reach a `Warm` verdict, and
/// nothing repeats that: the runtime gate is `keys_cached()`, which is
/// `vk.is_some() && the pk file exists`, and `KeyManagerState::new`
/// deliberately skips the proving key's digest (four constructors, and the
/// verifier daemon never loads a proving key at all).
///
/// Between that check and `load_pk` in stage 5 sit the burn and up to ~91
/// minutes of anchor wait. A key replaced inside that window is not exotic
/// — an rsync, a second keygen, a restored backup — and it is not reliably
/// caught later: `try_load_pk` is panic-wrapped so a TRUNCATED key reports,
/// but a same-length corruption past the embedded verifying key
/// deserialises happily and leaves `transcript_repr()` unchanged (see the
/// note in `keys/state.rs`). Warm, wrong, and the proof fails after the
/// money has moved.
///
/// **A digest, and not `(len, mtime, inode)`.** That was the first shape of
/// this check, on the theory that a replaced file always lands on a new
/// inode. It does not: a test replacing a 16-byte file got the SAME inode
/// back, because the filesystem reuses one it has just freed. Pair that
/// with a same-length write inside one mtime tick and the cheap check
/// passes over exactly the accident it was added for. Re-streaming the key
/// costs one sequential read on a path that has already spent minutes
/// proving, and it is the only version that is actually true.
///
/// The digest compared against is the one recorded HERE, not the manifest
/// read again at stage 5 — replacing the key and its manifest together is
/// a case the second form would wave through.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkFingerprint {
    path: PathBuf,
    sha256: String,
}

/// Refuse if the proving key is not the file preflight verified.
///
/// Called at the top of stage 5, after the burn — so this is exit 12
/// (`ProofFailed`), not a preflight refusal. Nothing is lost by stopping
/// here: a proof built from a key nobody vouched for either fails its own
/// self-verification or is rejected on submit, and both of those cost the
/// anchor wait first.
pub fn recheck_proving_key(before: Option<&PkFingerprint>) -> CliResult<()> {
    let Some(before) = before else {
        // Stage 1 found no warm cache, so there is no verified file to
        // compare against — stage 5 generates one under the keygen lock.
        return Ok(());
    };
    match bridge_prover_lib::keys::sha256_of(&before.path) {
        Ok(now) if now == before.sha256 => Ok(()),
        Ok(now) => Err(CliError::ProofFailed {
            reason: format!(
                "{} is not the proving key preflight verified: sha256 {}…, was {}…. That digest \
                 was streamed before the burn and nothing has re-verified it since, so a \
                 replacement is vouched for by nothing — and a proof built from it fails on \
                 submit, after the anchor wait.\n\x20 The AN burn is already on the wire. Restore \
                 the key this run started with, or clear the cache and re-run with --allow-retry, \
                 which regenerates and resumes from the recorded burn instead of making a second \
                 one:\n\x20   cargo run --release --manifest-path \
                 ../bridge-prover-libraries/Cargo.toml -p bridge-prover-lib --bin \
                 probe_event_keys -- --params-dir '{}' --repair",
                before.path.display(),
                &now[..8.min(now.len())],
                &before.sha256[..8.min(before.sha256.len())],
                before
                    .path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .display()
                    .to_string()
                    .replace('\'', r"'\''"),
            ),
            source: None,
        }),
        Err(e) => Err(CliError::ProofFailed {
            reason: format!(
                "{} cannot be read back ({e}): preflight verified it before the burn.\n\x20 The \
                 AN burn is already on the wire. Re-run with --allow-retry — a missing key reads \
                 as a cold cache, so stage 5 regenerates and resumes from the recorded burn \
                 rather than making a second one.",
                before.path.display(),
            ),
            source: None,
        }),
    }
}

/// The KZG ceremony, resolved exactly the way the prover will resolve it —
/// **at every degree the prover will ask for**, not just the largest.
///
/// The loop is the point. `probe_ceremony(dir, 21)` succeeding says nothing
/// about `kzg_bn254_20.srs`, and `load_srs(dir, 20)` will consult that exact
/// filename before it considers downsizing the K=21 file. See
/// [`REQUIRED_CEREMONY_KS`] for where each degree comes from.
pub fn check_ceremony(params_dir: &Path) -> CliResult<()> {
    for k in REQUIRED_CEREMONY_KS {
        probe_ceremony(params_dir, k).map_err(|e| CliError::Preflight {
            reason: format!(
                // `{e:#}` and NOT `{e}`. `resolve_ceremony` wraps the
                // verdict in the offending path, so `{e}` prints the path
                // alone and the operator is told "no usable ceremony" —
                // for a file that IS there and IS loadable and whose only
                // defect is that its tau is public. The sentence that
                // matters ("every proof produced with it is forgeable")
                // is the source, and only `:#` walks the chain to it.
                "--params-dir {}: the ceremony this build loads at k={k} is unusable:\n\
                 \x20   {e:#}\n\
                 \x20 If that says the file is NOT Hermez, it is not merely unusable: its toxic \
                 waste is public, so every proof produced with it is forgeable. DELETE the file \
                 named above — provisioning does not replace it, because the loader prefers \
                 kzg_bn254_{k}.srs by name over downsizing a larger ceremony.\n\
                 \x20 A withdrawal loads k={:?} — one bad or non-Hermez file at any of them \
                 panics the prover after the burn.\n\
                 \x20 Provision once (~2.4 GB download, then a few minutes of CPU):\n\
                 \x20   mkdir -p ~/.cache/halo2-kzg-srs\n\
                 \x20   curl -L --fail --progress-bar \\\n\
                 \x20     https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau \\\n\
                 \x20     -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau\n\
                 \x20   cd ../bridge-prover-libraries   # from crates/ackinacki-bridge/\n\
                 \x20   cargo build --release --bin bootstrap_hermez_srs\n\
                 \x20   ./target/release/bootstrap_hermez_srs --k 21 --params-dir {}\n\
                 \x20 That writes kzg_bn254_21.srs (~256 MB); lower degrees are derived from it \
                 on first use.\n\
                 \x20 If k=21 is fine and a lower degree is not, you have a stale or foreign \
                 kzg_bn254_{k}.srs — delete it and let it be re-derived.\n\
                 \x20 NOTE: scripts/bootstrap_hermez_srs.sh is a different tool — it writes K=20 \
                 into crates/bridge-snark-utils/params/.",
                params_dir.display(),
                REQUIRED_CEREMONY_KS,
                params_dir.display(),
            ),
            source: None,
        })?;
    }
    Ok(())
}

/// One `--help` run, bounded by `timeout`. Factored out so the ETXTBSY
/// retry above has a single thing to repeat.
async fn spawn_help(
    release_bin: &Path,
    aggregator_dir: &Path,
    timeout: std::time::Duration,
) -> Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed> {
    tokio::time::timeout(
        timeout,
        tokio::process::Command::new(release_bin)
            .arg("--help")
            .current_dir(aggregator_dir)
            // Without this, a timeout cancels the future and leaves the
            // child running — orphaned, holding the pipes, for as long as
            // it likes. `timeout` bounds our wait, not its life.
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
}

/// The committed verifier bytecode stage 1 compares with the verifier
/// deployed on the bridge.
pub fn check_verifier_bin(verifiers_dir: &Path, allow_drift: bool) -> CliResult<()> {
    check_pinned_verifier_file(
        verifiers_dir,
        WITHDRAW_VERIFIER_BIN,
        EXPECTED_VERIFIER_BIN,
        "stage 1 compares this bytecode with the verifier deployed on the bridge",
        allow_drift,
    )
}

/// The committed verifier source `aggregate-proof` self-checks against.
///
/// This file replaced a compiler. The aggregator used to compile the verifier
/// it generated and compare bytecode, which put `solc` on every withdrawing
/// host; it now compares the generated source, which the key fully determines.
/// So the source has to be here instead, and absent or stale it fails the run
/// at stage 5, after the burn.
pub fn check_verifier_sol(verifiers_dir: &Path, allow_drift: bool) -> CliResult<()> {
    check_pinned_verifier_file(
        verifiers_dir,
        WITHDRAW_VERIFIER_SOL,
        EXPECTED_VERIFIER_SOL,
        "aggregate-proof compares the verifier source it regenerates with this file at stage 5, \
         after the burn",
        allow_drift,
    )
}

/// Refuse a verifier file that is absent, unreadable, or not the one embedded
/// in this build. `role` says what the file is for, in the words the operator
/// needs to judge the refusal.
fn check_pinned_verifier_file(
    verifiers_dir: &Path,
    file: &str,
    expected: &[u8],
    role: &str,
    allow_drift: bool,
) -> CliResult<()> {
    let path = verifiers_dir.join(file);
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() => {},
        _ => {
            return Err(CliError::Preflight {
                reason: format!(
                    "--verifiers-dir {}: missing {file} — {role}, and the run cannot finish \
                     without it",
                    verifiers_dir.display(),
                ),
                source: None,
            })
        },
    }

    let on_disk = std::fs::read(&path).map_err(|e| CliError::Preflight {
        reason: format!(
            "--verifiers-dir {}: cannot read {file}: {e}",
            verifiers_dir.display()
        ),
        source: None,
    })?;
    if on_disk != expected && !allow_drift {
        return Err(CliError::Preflight {
            reason: format!(
                "--verifiers-dir {}: {file} is not the one this build expects ({} bytes on disk / \
                 sha256 {}, vs {} bytes / {} embedded) — {role}, so a mismatch surfaces later, \
                 after the burn.\n\x20 Running your own bridge deploy? Point --verifiers-dir at \
                 your own verifiers directory and pass --allow-verifier-drift; see the advanced \
                 runbook.",
                verifiers_dir.display(),
                on_disk.len(),
                short_sha256(&on_disk),
                expected.len(),
                short_sha256(expected),
            ),
            source: None,
        });
    }
    Ok(())
}

/// First 8 hex chars of a SHA-256 — enough to tell two artefacts apart in an
/// error message without printing 64 characters of noise.
fn short_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(bytes)[..4])
}

/// Prove we can actually aggregate, not merely that a file with the right
/// name exists.
///
/// `SubprocessAggregator` picks `target/release/aggregate-proof` on
/// `is_file()` alone and only falls back to `cargo run --release` when that
/// path is absent (`aggregator.rs:377`, `:432`). So a non-executable stub,
/// or a binary built for another architecture, is *selected over* a working
/// cargo build and then fails after the burn. Existence is not the property
/// we need; runnability is — hence the `--help` smoke run.
///
/// **Requires a prebuilt binary; the cargo fallback is refused here.** The
/// runtime will happily `cargo run --release` when the binary is missing,
/// but preflight cannot verify that path without paying for a cold build
/// inside a check whose whole purpose is to be instant — and "the crate
/// looks present" is not verification. Refusing early with the build command
/// costs an operator one command; accepting an unverified fallback costs
/// them the burn and up to 91 minutes. This is deliberately stricter than
/// the runtime.
pub async fn check_aggregator_runnable(aggregator_dir: &Path) -> CliResult<()> {
    check_aggregator_runnable_with_timeout(aggregator_dir, AGGREGATOR_PROBE_TIMEOUT).await
}

/// The probe, with the wait as a parameter.
///
/// Split out for the timeout test: the alternative is a unit test that
/// sleeps for the production 30 s, which every future run then pays.
pub(crate) async fn check_aggregator_runnable_with_timeout(
    aggregator_dir: &Path,
    // Plain `//`: doc comments cannot be applied to parameters.
    timeout: Duration,
) -> CliResult<()> {
    let refuse = |reason: String| CliError::Preflight {
        reason,
        source: None,
    };

    // Canonicalise BEFORE spawning, because the spawn sets `current_dir` to
    // this same directory and a child resolves a relative program path
    // against its OWN cwd — so `--aggregator-dir ./agg` would try to exec
    // `./agg/agg/target/release/aggregate-proof`. `is_file()` below is
    // evaluated against the parent's cwd and passes, which makes the failure
    // look like a broken binary rather than a path bug.
    //
    // This is not a corner case: the shipped profile sets
    // `BRIDGE_AGGREGATOR_DIR=../bridge-evm-aggregator`, relative by design.
    //
    // The runtime does not have this bug: `SubprocessAggregator::new`
    // canonicalises `aggregator_dir` at construction, so by the time
    // `release_bin` joins onto it the path is already absolute. Preflight
    // has no such step, which is why it needs this one.
    let relative_bin = aggregator_dir.join("target/release/aggregate-proof");
    if !relative_bin.is_file() {
        return Err(refuse(format!(
            "--aggregator-dir {}: no target/release/aggregate-proof.\n\x20 Build it first — this \
             CLI will not fall back to `cargo run`, because a cold build cannot be verified \
             inside a preflight and an unverified aggregator costs the burn:\n\x20   cd {} && \
             cargo build --release --bin aggregate-proof",
            aggregator_dir.display(),
            aggregator_dir.display(),
        )));
    }

    // Resolve now, while the cwd is still ours.
    let release_bin = relative_bin.canonicalize().map_err(|e| {
        refuse(format!(
            "--aggregator-dir {}: could not resolve {}: {e}",
            aggregator_dir.display(),
            relative_bin.display(),
        ))
    })?;

    // ETXTBSY is retried, and only ETXTBSY.
    //
    // Linux refuses to exec a file that any process has open for writing.
    // The obvious case — a `cargo build` still running — is a real one an
    // operator hits by pipelining build and run. The less obvious case is
    // a fork/exec race inside a multi-threaded process: a concurrently
    // spawning thread's child inherits the writer's descriptor for the
    // window between fork and exec, and this exec fails while that window
    // is open. It made this crate's own suite fail intermittently under
    // parallel test threads, and it would do the same to an operator's
    // scripted build-then-withdraw.
    //
    // Reporting that as "wrong architecture, or not executable" — which is
    // what the arm below says — sends them to rebuild a binary that is
    // fine. Three tries over ~300 ms costs nothing on the path that
    // matters and removes the misdiagnosis.
    let mut run = spawn_help(&release_bin, aggregator_dir, timeout).await;
    for _ in 0..2 {
        let busy = matches!(
            &run,
            Ok(Err(e)) if e.raw_os_error() == Some(libc::ETXTBSY)
        );
        if !busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        run = spawn_help(&release_bin, aggregator_dir, timeout).await;
    }

    match run {
        // Not just "exited 0" — a stub that ignores its arguments does that
        // too. The help text must mention the flag we will actually pass,
        // which only the real binary does.
        Ok(Ok(out))
            if out.status.success()
                && help_mentions_our_flags(&out)
                && help_mentions_source_self_check(&out) =>
        {
            Ok(())
        },
        // `--inner-snark` alone is not enough: the old aggregate-proof prints
        // that too, and with the solc probe gone this CLI can no longer tell
        // "old binary" from "new binary" by exit status. `--allow-source-drift`
        // only exists in the new usage line, so its absence is the tell —
        // and the fix is not a rebuild flag but a rebuild, because the old
        // binary would still reach `compile_solidity` at stage 5 and needs
        // solc there, which this CLI no longer provisions.
        Ok(Ok(out)) if out.status.success() && help_mentions_our_flags(&out) => {
            Err(refuse(format!(
                "--aggregator-dir {}: {} predates the verifier-source self-check — its `--help` \
                 does not mention `--allow-source-drift` — and would still need solc on PATH at \
                 stage 5. Rebuild it:\n\x20   cd {} && cargo build --release --bin aggregate-proof",
                aggregator_dir.display(),
                release_bin.display(),
                aggregator_dir.display(),
            )))
        },
        Ok(Ok(out)) if out.status.success() => Err(refuse(format!(
            "--aggregator-dir {}: {} answered `--help` but its output does not mention \
             `--inner-snark` — this is not the aggregate-proof this CLI drives. Rebuild it:\n\
             \x20   cd {} && cargo build --release --bin aggregate-proof",
            aggregator_dir.display(),
            release_bin.display(),
            aggregator_dir.display(),
        ))),
        Ok(Ok(out)) => Err(refuse(format!(
            "--aggregator-dir {}: {} exists but `--help` exited {} — a stale or \
             wrong-architecture build would fail only after the burn. Rebuild it:\n\x20   cd {} \
             && cargo build --release --bin aggregate-proof\n\x20 stderr: {}",
            aggregator_dir.display(),
            release_bin.display(),
            out.status,
            aggregator_dir.display(),
            String::from_utf8_lossy(&out.stderr).trim(),
        ))),
        // Still busy after the retries. Say what that actually is: the
        // remedy is to wait for the writer, not to rebuild.
        Ok(Err(e)) if e.raw_os_error() == Some(libc::ETXTBSY) => Err(refuse(format!(
            "--aggregator-dir {}: {} cannot be executed because another process still has it open \
             for writing (ETXTBSY). The binary is almost certainly fine — a build is probably \
             still running. Wait for it to finish and re-run; nothing was sent.",
            aggregator_dir.display(),
            release_bin.display(),
        ))),
        Ok(Err(e)) => Err(refuse(format!(
            "--aggregator-dir {}: {} exists but cannot be executed: {e} — wrong architecture, or \
             not executable. Rebuild it:\n\x20   cd {} && cargo build --release --bin \
             aggregate-proof",
            aggregator_dir.display(),
            release_bin.display(),
            aggregator_dir.display(),
        ))),
        Err(_) => Err(refuse(format!(
            "--aggregator-dir {}: {} did not respond to `--help` within {:?} (the process was \
             killed)",
            aggregator_dir.display(),
            release_bin.display(),
            timeout,
        ))),
    }
}

/// `--help` output must name a flag this CLI actually passes. Guards against
/// a stub or an unrelated binary parked at the expected path.
fn help_mentions_our_flags(out: &std::process::Output) -> bool {
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    text.contains("--inner-snark") || err.contains("--inner-snark")
}

/// `--help` output must name `--allow-source-drift`, which only the
/// verifier-source self-check era of `aggregate-proof` has. A binary whose
/// `--help` mentions `--inner-snark` but not this flag predates the
/// self-check and still needs `solc` at stage 5 — see the caller.
fn help_mentions_source_self_check(out: &std::process::Output) -> bool {
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    text.contains("--allow-source-drift") || err.contains("--allow-source-drift")
}

/// Output paths the prover will write to. Creating them now also means the
/// aggregator subprocess never races on mkdir.
///
/// `params_dir` is `Option` because whether it is an output at all depends
/// on the key cache. When the cache is cold the first Circuit-4 run calls
/// `ensure_event_keys`, whose `run_keygen` writes `event_vk.bin`,
/// `event_pk.bin` and `event_config_params.json` straight into `params_dir`
/// — ~2.65 GB of it — and a read-only `params/` then passes preflight and
/// fails after the burn. When the cache is warm nothing writes there, and
/// demanding write access would refuse a read-only or shared `params/` that
/// works perfectly well; the shipped profile describes exactly that setup
/// ("shared with the bundle daemon").
///
/// The caller decides, because the caller has already probed the cache.
///
/// Note also that every shipped profile sets `BRIDGE_PK_CACHE_DIR`
/// explicitly, so the `params_dir/pk_cache` default never fires — which is
/// why `params_dir` needs to appear here in its own right rather than being
/// probed incidentally through the pk-cache path.
///
/// This checks that the paths accept writes; [`check_disk_headroom`] checks
/// that they have room — summed per filesystem, because the shipped profile
/// puts one inside the other.
pub fn check_output_dirs(
    work_dir: &Path,
    snark_dir: &Path,
    // Already resolved against the `<params_dir>/pk_cache` default by the
    // caller — not an `Option`, because "unset" is not a state the runtime
    // has. Plain `//`: doc comments cannot be applied to parameters.
    pk_cache_dir: &Path,
    params_dir: Option<&Path>,
) -> CliResult<()> {
    let mut targets: Vec<(&'static str, &Path)> = vec![
        ("--work-dir", work_dir),
        ("--snark-dir", snark_dir),
        // Always. The caller resolved the default already, so this is the
        // real path either way and it is always written to.
        ("--pk-cache-dir", pk_cache_dir),
    ];
    // Keygen output, and only when keygen will actually run.
    if let Some(d) = params_dir {
        targets.push(("--params-dir", d));
    }
    for (flag, dir) in targets {
        ensure_writable_dir(flag, dir)?;
    }
    Ok(())
}

/// Create `dir` if absent and prove we can write into it. A read-only output
/// path is a stage-5 crash today; here it costs one temp file.
///
/// The probe name is unique per process and the file is opened with
/// `create_new`, so this can never truncate something that was already there
/// — a fixed name plus `fs::write` would happily destroy an operator's file
/// that happened to match. Cleanup failure is reported, not swallowed: a
/// probe we cannot remove is litter inside a directory the prover is about
/// to fill.
///
/// **Scope: writability.** One 4 KiB block proves the filesystem accepts
/// data. Capacity is checked separately and conditionally — see
/// [`check_disk_headroom`] — because the amount needed depends on what is
/// already cached.
fn ensure_writable_dir(flag: &'static str, dir: &Path) -> CliResult<()> {
    use std::io::Write;

    std::fs::create_dir_all(dir).map_err(|e| CliError::Preflight {
        reason: format!("{flag} {}: cannot create: {e}", dir.display()),
        source: None,
    })?;

    let probe = dir.join(format!(
        ".ackinacki-bridge-write-probe.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| CliError::Preflight {
            reason: format!("{flag} {}: not writable: {e}", dir.display()),
            source: None,
        })?;
    // Real bytes, then fsync. `write_all(b"")` loops zero times and issues
    // no syscall at all, so an empty probe proves only that a directory
    // entry could be created — it would sail through a filesystem that
    // cannot accept a single byte. One block, forced to disk, is the
    // smallest thing that actually exercises the write path.
    f.write_all(&[0u8; 4096]).map_err(|e| CliError::Preflight {
        reason: format!("{flag} {}: not writable: {e}", dir.display()),
        source: None,
    })?;
    f.sync_all().map_err(|e| CliError::Preflight {
        reason: format!("{flag} {}: write could not be flushed: {e}", dir.display()),
        source: None,
    })?;
    drop(f);

    std::fs::remove_file(&probe).map_err(|e| CliError::Preflight {
        reason: format!(
            "{flag} {}: wrote a probe file but could not remove it ({}): {e}",
            dir.display(),
            probe.display(),
        ),
        source: None,
    })?;
    Ok(())
}

/// Refuse when the run cannot fit what it is about to write.
///
/// **One check, not two, because free space is a property of the filesystem
/// and not of the directory.** The shipped profile puts the pk cache
/// *inside* params (`BRIDGE_PK_CACHE_DIR=…/params/pk_cache`,
/// `bridge_config.shellnet:68`), so two independent comparisons against the
/// same `statvfs` both pass at 3.5 GB free while the run needs ~3.65 GB —
/// and the shortfall lands during aggregation, after the burn. Two checks
/// that each ask "is there room for my piece?" never notice they are sharing
/// a pie.
///
/// So: group the requirements by device, sum them, compare once. When the
/// two directories are on different filesystems the sums are independent and
/// this degrades to the obvious thing.
///
/// Takes the already-probed `cache` rather than probing again: the caller
/// needs the same answer to decide whether `params_dir` must be writable,
/// and a probe is not cheap. Each one hashes the ~2.65 GB proving key and
/// then deserialises it, so probing twice would mean four passes over it in
/// preflight alone.
pub fn check_disk_headroom(
    params_dir: &Path,
    pk_cache_dir: &Path,
    cache: KeyCacheState,
) -> CliResult<()> {
    // Report litter before measuring, whether or not the measurement then
    // refuses. A killed keygen leaves its whole partial temp behind
    // (`NamedTempFile` cleans up on drop, not on SIGKILL), the name is a
    // dotfile so `ls` does not show it, and until recently nothing swept
    // it. A host can accumulate several of these and never be told.
    for l in leaked_keygen_temp_files(params_dir) {
        tracing::warn!(
            path = %l.path.display(),
            gb = l.bytes as f64 / 1e9,
            "an interrupted keygen left a temp file behind; it is holding space this run may \
             need. Remove it with `probe_event_keys --params-dir <dir> --repair`",
        );
    }

    for r in plan_disk_headroom(params_dir, pk_cache_dir, cache)? {
        require_space(r.probe, r.needed, &r.parts)?;
    }
    Ok(())
}

/// Everything the check DECIDES, decided — and nothing performed.
///
/// A seam because the deciding is what has to be right and the performing
/// needs a filesystem with a known amount of free space, which a test does
/// not have. The helpers below were each tested on their own for two
/// rounds while the composition — reading the two device ids at all, and
/// summing in the right direction — was reached by nothing: replacing
/// `shared` with a constant `false` passed the whole suite.
fn plan_disk_headroom<'a>(
    params_dir: &'a Path,
    pk_cache_dir: &'a Path,
    cache: KeyCacheState,
) -> CliResult<Vec<SpaceRequirement<'a>>> {
    // What each path needs, in bytes, on this run.
    let params_need = keygen_requirement(params_dir, cache)?;
    let pk_need = pk_cache_requirement(pk_cache_dir);

    // Group by device. `st_dev` is what makes two paths share a pool;
    // comparing the paths themselves would miss a bind mount and a symlink
    // both ways.
    let shared = share_a_filesystem(device_of(params_dir), device_of(pk_cache_dir));

    Ok(space_requirements(
        params_dir,
        params_need,
        pk_cache_dir,
        pk_need,
        shared,
    ))
}

/// Do these two `st_dev` readings mean one pool of free space?
///
/// Split out because the answer for the unreadable case is a decision, not
/// a detail: assume shared. Over-reserving refuses a run that would have
/// fit, which costs a flag; under-reserving loses a burn. Inlined in the
/// match it was invisible to the tests — flipping it to `false` left the
/// whole suite green.
fn share_a_filesystem(params: Option<u64>, pk_cache: Option<u64>) -> bool {
    match (params, pk_cache) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// One filesystem's worth of the answer: probe `probe`, insist on `needed`
/// bytes there, and name `parts` in the refusal so the operator can see
/// what adds up to the total.
#[derive(Debug)]
struct SpaceRequirement<'a> {
    probe: &'a Path,
    needed: u64,
    parts: Vec<(&'static str, u64)>,
}

/// The requirements this run must satisfy, grouped by the filesystem that
/// has to hold them.
///
/// This is the part of the check that has to be right — the grouping and
/// the summing — and it is pure, so it can be asserted without controlling
/// anyone's free space. `require_space` then performs each one.
fn space_requirements<'a>(
    params_dir: &'a Path,
    params_need: u64,
    pk_cache_dir: &'a Path,
    pk_need: u64,
    shared: bool,
) -> Vec<SpaceRequirement<'a>> {
    if shared {
        // One comparison against one `statvfs`. Two independent ones both
        // pass at 3.5 GB free while the run needs ~3.65 GB.
        vec![SpaceRequirement {
            probe: params_dir,
            needed: params_need + pk_need,
            parts: vec![("--params-dir", params_need), ("--pk-cache-dir", pk_need)],
        }]
    } else {
        vec![
            SpaceRequirement {
                probe: params_dir,
                needed: params_need,
                parts: vec![("--params-dir", params_need)],
            },
            SpaceRequirement {
                probe: pk_cache_dir,
                needed: pk_need,
                parts: vec![("--pk-cache-dir", pk_need)],
            },
        ]
    }
}

/// `st_dev` for `path`, or `None` if it cannot be read.
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

/// Bytes the Circuit-4 keygen will write into `params_dir` on this run:
/// `EVENT_KEYGEN_BYTES` when the cache is cold, zero when it is warm, and a
/// refusal when it is unusable.
fn keygen_requirement(params_dir: &Path, cache: KeyCacheState) -> CliResult<u64> {
    match cache {
        // Keygen is skipped and the space is already spent. A warm host is
        // not asked to keep 3 GB free forever.
        KeyCacheState::Warm => Ok(0),
        KeyCacheState::Corrupt {
            why,
        } => Err(CliError::Preflight {
            reason: format!(
                "--params-dir {}: the Circuit-4 key cache cannot be trusted: {why}.\n\x20 \
                 Clearing it is safe — the next run regenerates (~7 min, ~3 GB). Use the tool \
                 rather than four rm paths, so the manifest goes first and a half-finished clear \
                 cannot read as a cache hit:\n\x20   cargo run --release --manifest-path \
                 ../bridge-prover-libraries/Cargo.toml -p bridge-prover-lib --bin \
                 probe_event_keys -- --params-dir '{}' --repair\n\x20 (run from \
                 crates/ackinacki-bridge/; the quotes matter if your path contains spaces)",
                params_dir.display(),
                // Single-quoted above. A bare {} splits a path with a space
                // into two arguments, and the operator's copy-paste fails
                // with a confusing "unknown argument".
                params_dir.display().to_string().replace('\'', r"'\''"),
            ),
            source: None,
        }),
        // Blocked: the same exit code as `Corrupt` and a deliberately
        // different sentence. The `Corrupt` arm above promises "clearing it
        // is safe" and hands over a `--repair` command; both are false here,
        // and printing them would send an operator to a tool that refuses.
        // Nothing to add to `why` — the probe already named the path and
        // said what to do with it.
        KeyCacheState::Blocked {
            why,
        } => Err(CliError::Preflight {
            reason: format!("--params-dir {}: {why}", params_dir.display()),
            source: None,
        }),
        // Cold: keygen will run, so the room has to be there.
        KeyCacheState::Cold {
            why,
        } => {
            tracing::info!(
                params_dir = %params_dir.display(),
                %why,
                "Circuit-4 keys will be generated on this run",
            );
            Ok(EVENT_KEYGEN_BYTES)
        },
    }
}

/// Bytes the outer aggregator keygen may write into the pk cache.
///
/// Always `OUTER_PK_BYTES`, and the argument is deliberately unused: the
/// slot's name is a content hash over the inner proof, which does not exist
/// until stage 5, so there is nothing in the directory this function could
/// match against to narrow the figure. The parameter stays in the signature
/// because the day the slot name becomes predictable this is where the
/// narrowing goes.
fn pk_cache_requirement(_pk_cache_dir: &Path) -> u64 {
    OUTER_PK_BYTES
}

/// Compare one filesystem's free space against the sum of what will be
/// written to it, and name the parts in the refusal.
///
/// `probe` only picks the filesystem — every path in `parts` must be on it.
fn require_space(probe: &Path, needed: u64, parts: &[(&str, u64)]) -> CliResult<()> {
    if needed == 0 {
        return Ok(());
    }
    let Some(available) = available_bytes(probe) else {
        // statvfs failed (unusual mount, container quirk). Do not invent a
        // refusal out of a failed measurement.
        tracing::warn!(
            path = %probe.display(),
            "could not determine free space; skipping the headroom check",
        );
        return Ok(());
    };
    if available >= needed {
        return Ok(());
    }

    // Itemise. "needs 4.3 GB" invites the operator to free 4.3 GB and be
    // surprised; "3.0 for keygen + 0.9 for the aggregator, same filesystem"
    // tells them what is actually happening.
    let breakdown = parts
        .iter()
        .filter(|(_, b)| *b > 0)
        .map(|(name, b)| format!("{name} {:.1} GB", *b as f64 / 1e9))
        .collect::<Vec<_>>()
        .join(" + ");

    // "Free 4.0 GB" is the wrong instruction when 2.65 GB of the shortfall
    // is a temp file the operator cannot see. Name it, and name the tool,
    // before telling them to go find space elsewhere.
    let leaks = leaked_keygen_temp_files(probe);
    let reclaimable = if leaks.is_empty() {
        String::new()
    } else {
        let total: u64 = leaks.iter().map(|l| l.bytes).sum();
        format!(
            "\n\x20 {:.1} GB of that is {} interrupted-keygen temp file(s) still sitting in {} — \
             hidden, because the names start with a dot, and nothing removes them on its \
             own:\n\x20   cargo run --release --manifest-path \
             ../bridge-prover-libraries/Cargo.toml -p bridge-prover-lib --bin probe_event_keys -- \
             --params-dir '{}' --repair\n\x20 (run from crates/ackinacki-bridge/; reclaim that \
             first, it may be all you need)",
            total as f64 / 1e9,
            leaks.len(),
            probe.display(),
            probe.display().to_string().replace('\'', r"'\''"),
        )
    };

    Err(CliError::Preflight {
        reason: format!(
            "{}: {:.1} GB free, but this run needs {:.1} GB there ({breakdown}). These paths \
             share one filesystem, so the requirements add up — checking them separately is how a \
             host with room for either and not both reaches stage 5 and fails after the burn. \
             Free space, or move --pk-cache-dir to another filesystem.{reclaimable}",
            probe.display(),
            available as f64 / 1e9,
            needed as f64 / 1e9,
        ),
        source: None,
    })
}

/// Free space at `path`, or `None` if it cannot be measured.
///
/// Uses `libc::statvfs`. `libc` is already in this workspace's lock file, so
/// depending on it directly costs nothing — and it beats hand-declaring the
/// struct, whose layout is platform-specific.
fn available_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path; `stat` is written only on
    // success, and we read it only then.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    // f_bavail: blocks available to a non-privileged process.
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

// -- EVM-side preflight ------------------------------------------------------

/// The verifier bytecode stage 1 compares with the Yul runtime deployed on the
/// bridge.
pub(crate) const WITHDRAW_VERIFIER_BIN: &str = "BridgeWithdrawalAggregatorVerifier.bin";

/// The verifier source `aggregate-proof` self-checks its output against; it
/// joins `{name}.sol` onto `--verifiers-dir`.
pub(crate) const WITHDRAW_VERIFIER_SOL: &str = "BridgeWithdrawalAggregatorVerifier.sol";

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
            got: crate::errors::Redacted::rendered(format!("<redacted {} chars>", raw.len())),
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
        got: crate::errors::Redacted::rendered(format!("{} ({e})", crate::args::redact(rpc_url))),
    })?;
    let provider = ProviderBuilder::new().connect_http(url);
    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|e| CliError::Preflight {
            reason: format!("--rpc-url {rpc_url}: eth_chainId failed: {e}"),
            source: Some(anyhow::Error::new(e)),
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
            source: Some(anyhow::Error::new(e)),
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
        got: crate::errors::Redacted::rendered(format!("{} ({e})", crate::args::redact(rpc_url))),
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
            source: Some(anyhow::Error::new(e)),
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
            source: Some(anyhow::Error::new(e)),
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
                    source: Some(anyhow::Error::new(e)),
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
                source: Some(anyhow::Error::new(e)),
            })?;
    if on_chain_identity != expected_identity {
        return Err(CliError::Preflight {
            reason: format!(
                // `{:064x}`, not `U256`'s `Display`, and no `0x` prefix: the
                // last line sends the operator to `USDC_BRIDGE_ACCOUNT_ID`,
                // which is written in the profile as 64 lowercase hex
                // characters with the leading zeros kept
                // (`config/bridge_config.shellnet:65`). Decimal is the one
                // rendering that cannot be compared against it by eye, so
                // the refusal asked for a check it made impossible — and it
                // did so next to `preflight ok usdc_bridge=…`, which prints
                // the same pair as hex.
                "--bridge-address {bridge} is pinned to a different Acki Nacki bridge account: on \
                 chain (dappFr, accFr) = ({:064x}, {:064x}), this withdrawal would prove \
                 ({:064x}, {:064x}). withdrawByProof reverts WithdrawIdentityMismatch before it \
                 even verifies the proof.\n\x20 Check USDC_BRIDGE_ACCOUNT_ID in $BRIDGE_CONFIG \
                 against the bridge you are withdrawing from.",
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
            source: Some(anyhow::Error::new(e)),
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

// `pub(crate)` so `test_chain` can build the EVM half of a fake world
// out of `mock_rpc_code_for` + `full_walk`. That pair is the only answer
// set in this crate that gets `check_bridge_deploy` all the way to `Ok`,
// and a second copy of a selector table is a second thing to drift.
#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::source_guard::production_source;

    /// The AN half of stage 1, driven end to end against a fake node.
    ///
    /// Everything before this was a helper tested in isolation. This is
    /// `preflight::run` itself — the key file's mode, the account fetch,
    /// the Active and code-hash checks, `getCustodians` on the account's
    /// own code, the owner match against `--from-keys`, the USDCBridge
    /// lookup and the ECC[3] sufficiency check — with nothing stubbed
    /// but the wire.
    #[tokio::test]
    async fn preflight_passes_its_an_side_against_a_fake_node() {
        let (account_id, account_boc) = crate::test_chain::deployed_multisig(5_000_000).await;
        let usdc_bridge_account_id = "2b".repeat(32);
        let node = crate::test_chain::fake_node(crate::test_chain::NodeFixture {
            account_id: account_id.clone(),
            account_boc,
            usdc_bridge_account_id: usdc_bridge_account_id.clone(),
        })
        .await;

        let dir = tempfile::TempDir::new().unwrap();
        let keys = write_keys(dir.path(), PAIR_PUBLIC, PAIR_SECRET);
        set_owner_only(&keys);

        let from = crate::args::parse_from(&format!("{account_id}::{account_id}")).unwrap();
        let to =
            crate::args::parse_to("0x742d35Cc6634C0532925a3b844Bc454e4438f44e", Some(11155111))
                .unwrap();
        let amount = crate::args::parse_amount("1.000000").unwrap();

        let report = run(
            &from,
            &keys,
            &to,
            &amount,
            &node.url,
            &usdc_bridge_account_id,
            BalanceCheck::Require,
        )
        .await
        .expect("the AN side of preflight passes against a real account and a fake wire");

        assert_eq!(
            report.multisig_ecc3_balance, 5_000_000,
            "the balance comes off the account BOC, not off anything the node said in JSON",
        );
        assert_eq!(
            report.usdc_bridge_extended,
            format!("{usdc_bridge_account_id}::{usdc_bridge_account_id}"),
        );

        // And it got there by asking, rather than by some default
        // answering for it. The two REST shapes are the ones this crate
        // would silently stop needing if `get_account` changed form.
        let seen = node.seen.lock().await.join("\n");
        assert!(
            seen.contains("/v2/account"),
            "the account was fetched: {seen}"
        );
        assert!(
            seen.contains("POST /graphql"),
            "the USDCBridge was queried: {seen}"
        );
    }

    /// `check_key_file_perms` wants exactly `0400`, and `write_keys`
    /// leaves whatever the umask gives.
    fn set_owner_only(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
    }

    /// The check no hand-written fixture can pass: `getCustodians` is
    /// not a field read off a JSON blob, it is TVM executed against the
    /// account's own code. If this passes, the offline fixture is a real
    /// multisig and everything downstream of preflight can be driven
    /// from it.
    #[tokio::test]
    async fn the_offline_fixture_answers_getcustodians_with_the_test_key() {
        let (account_id, boc) = crate::test_chain::deployed_multisig(5_000_000).await;
        let ctx = build_client_context("http://127.0.0.1:1/graphql")
            .expect("a context needs no reachable endpoint to run TVM locally");

        let custodians = call_get_custodians(&ctx, &format!("0:{account_id}"), &boc)
            .await
            .expect("getCustodians runs on the deployed account");

        assert_eq!(
            custodians.len(),
            1,
            "single-custodian is the only shape this CLI can sign for: {custodians:?}",
        );
        let owner = custodians[0]
            .owner_pubkey_hex
            .as_deref()
            .expect("a pubkey-owned custodian");
        assert!(
            pubkeys_equal(PAIR_PUBLIC, owner),
            "the fixture has to be owned by the key the tests sign with, or every run built on it \
             fails at the owner match instead of where the test is aiming: {owner}",
        );
    }

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
        assert_eq!(
            describe_account_status(AccountStatus::AccStateActive),
            "Active"
        );
        assert_eq!(
            describe_account_status(AccountStatus::AccStateUninit),
            "Uninit"
        );
        assert_eq!(
            describe_account_status(AccountStatus::AccStateFrozen),
            "Frozen"
        );
        assert_eq!(
            describe_account_status(AccountStatus::AccStateNonexist),
            "NonExist"
        );
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
    pub(crate) const SEL_DAPP_FR: &str = "e20b7f65"; // bridgeWithdrawalDappFr()
    pub(crate) const SEL_ACC_FR: &str = "5c987786"; // bridgeWithdrawalAccFr()
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
    pub(crate) async fn mock_rpc_code_for(
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
                    // Serve every request on the connection, not just the
                    // first. `check_bridge_deploy` makes several calls in a
                    // row (eth_getCode, then the getter walk), and reqwest
                    // keeps the connection alive between them. A handler
                    // that answered once and returned closed the socket
                    // under the client mid-sequence: the run then failed
                    // with a transport error instead of the refusal the
                    // test asserts on, intermittently and only under
                    // parallel load. `a_zero_withdrawal_verifier_is_refused`
                    // was the one that showed it.
                    let mut buf = vec![0u8; 8192];
                    loop {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return; // client hung up
                        }
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
                        let payload = serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
                            .to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
                             {}\r\n\r\n{}",
                            payload.len(),
                            payload
                        );
                        if sock.write_all(resp.as_bytes()).await.is_err() {
                            return;
                        }
                    }
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

    /// The full four-address walk every late-stage test needs: bridge (1)
    /// → adapter (2) → wrapper (3) → yul (4), each answering its getter
    /// and each carrying code. Callers override the entries they are
    /// actually testing.
    ///
    /// Written as a helper because a mock that stops short fails on an
    /// empty `eth_call` decode long before the assertion, and the test is
    /// then green or red for a reason it does not name.
    pub(crate) fn full_walk(
        overrides: &[(&'static str, String)],
    ) -> std::collections::HashMap<&'static str, String> {
        let mut m = std::collections::HashMap::from([
            (SEL_VERIFIER, word_addr(2)),
            (SEL_SHPLONK, word_addr(3)),
            (SEL_YUL, word_addr(4)),
            (SEL_DAPP_FR, ZERO_WORD.to_string()),
            (SEL_ACC_FR, ZERO_WORD.to_string()),
            (SEL_TREASURY, MAX_WORD.to_string()),
        ]);
        for (k, v) in overrides {
            m.insert(k, v.clone());
        }
        m
    }

    /// The four addresses of [`full_walk`], all carrying code.
    pub(crate) fn full_walk_code() -> Vec<(Address, &'static str)> {
        (1u8..=4)
            .map(|n| (Address::repeat_byte(n), SOME_CODE))
            .collect()
    }

    // -- Disk headroom ---------------------------------------------------
    //
    // `check_disk_headroom` and its four helpers had no tests at all, and
    // they are what stands between a host with room for either artefact
    // and not both, and a shortfall that lands in stage 5 — after the
    // burn. The free-space number itself cannot be arranged portably, so
    // these test the parts that decide what to compare, plus the message.

    // -- The proving key between stage 1 and stage 5 ---------------------

    fn plant_pk(dir: &Path, bytes: &[u8]) -> PkFingerprint {
        let path = dir.join("event_pk.bin");
        std::fs::write(&path, bytes).unwrap();
        let sha256 = bridge_prover_lib::keys::sha256_of(&path).unwrap();
        PkFingerprint {
            path,
            sha256,
        }
    }

    #[test]
    fn an_untouched_proving_key_passes_the_stage_5_recheck() {
        let dir = tempfile::TempDir::new().unwrap();
        let before = plant_pk(dir.path(), b"proving key");
        recheck_proving_key(Some(&before)).expect("the same bytes are the same key");
    }

    #[test]
    fn a_cold_cache_has_nothing_to_recheck() {
        // Stage 1 records a digest only for a `Warm` verdict, because only
        // that one streamed the key's bytes. A cold run generates the key
        // in stage 5 under the keygen lock, and there is no earlier file to
        // compare against — refusing here would break every first run on a
        // fresh host.
        recheck_proving_key(None).expect("nothing was verified, so nothing changed");
    }

    #[test]
    fn a_replaced_proving_key_is_refused_after_the_burn() {
        let dir = tempfile::TempDir::new().unwrap();
        let before = plant_pk(dir.path(), b"the key preflight verified");
        std::fs::write(dir.path().join("event_pk.bin"), b"something else entirely").unwrap();

        let err = recheck_proving_key(Some(&before))
            .expect_err("a key nobody verified must not be proved with");
        let msg = format!("{err}");
        assert!(
            matches!(err, CliError::ProofFailed { .. }),
            "exit 12: {msg}"
        );
        assert!(
            msg.contains("not the proving key preflight verified"),
            "got: {msg}"
        );
        assert!(
            msg.contains("already on the wire"),
            "must say the burn happened: {msg}"
        );
        assert!(
            msg.contains("--allow-retry"),
            "must give a way out that does not re-burn: {msg}"
        );
    }

    #[test]
    fn a_same_length_replacement_is_still_caught() {
        // This is why the check is a digest and not `(len, mtime, inode)`.
        // The first version of it assumed a replaced file lands on a fresh
        // inode; it does not — the filesystem reuses one it has just
        // freed, and this exact test caught that with `left: 633990,
        // right: 633990`. Pair inode reuse with a same-length write inside
        // one mtime tick and the cheap check waves through precisely the
        // accident it exists for.
        let dir = tempfile::TempDir::new().unwrap();
        let before = plant_pk(dir.path(), b"AAAAAAAAAAAAAAAA");
        std::fs::remove_file(dir.path().join("event_pk.bin")).unwrap();
        std::fs::write(dir.path().join("event_pk.bin"), b"BBBBBBBBBBBBBBBB").unwrap();

        let md = std::fs::metadata(dir.path().join("event_pk.bin")).unwrap();
        assert_eq!(md.len(), 16, "same length, by construction");
        assert!(
            recheck_proving_key(Some(&before)).is_err(),
            "equal length must not be read as equal file"
        );
    }

    #[test]
    fn a_vanished_proving_key_is_refused_and_says_how_to_resume() {
        let dir = tempfile::TempDir::new().unwrap();
        let before = plant_pk(dir.path(), b"proving key");
        std::fs::remove_file(dir.path().join("event_pk.bin")).unwrap();

        let err = recheck_proving_key(Some(&before)).expect_err("the key is gone");
        let msg = format!("{err}");
        assert!(
            matches!(err, CliError::ProofFailed { .. }),
            "exit 12: {msg}"
        );
        assert!(msg.contains("cannot be read back"), "got: {msg}");
        assert!(
            msg.contains("--allow-retry"),
            "the burn already happened; the way forward must not re-burn: {msg}"
        );
    }

    #[test]
    fn a_swapped_symlink_target_is_caught() {
        // Streaming the bytes reads through the link, so a repointed
        // symlink is simply different content — no separate rule needed.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"first").unwrap();
        std::fs::write(dir.path().join("b.bin"), b"second").unwrap();
        std::os::unix::fs::symlink("a.bin", dir.path().join("event_pk.bin")).unwrap();
        let path = dir.path().join("event_pk.bin");
        let before = PkFingerprint {
            sha256: bridge_prover_lib::keys::sha256_of(&path).unwrap(),
            path,
        };

        std::fs::remove_file(dir.path().join("event_pk.bin")).unwrap();
        std::os::unix::fs::symlink("b.bin", dir.path().join("event_pk.bin")).unwrap();
        assert!(
            recheck_proving_key(Some(&before)).is_err(),
            "the link now resolves to different bytes"
        );
    }

    #[test]
    fn a_warm_cache_asks_for_no_keygen_room() {
        // A host that already generated the keys is not asked to keep 3 GB
        // free forever. Getting this wrong makes every warm run refuse.
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            keygen_requirement(dir.path(), KeyCacheState::Warm).unwrap(),
            0
        );
    }

    #[test]
    fn a_cold_cache_reserves_the_keygen() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            keygen_requirement(dir.path(), KeyCacheState::Cold {
                why: "no manifest".into()
            })
            .unwrap(),
            EVENT_KEYGEN_BYTES,
        );
    }

    #[test]
    fn corrupt_and_blocked_refuse_with_different_advice() {
        // The distinction is the point. `--repair` clears the cache, which
        // is right for a cache that contradicts itself and wrong for one
        // with a directory sitting on a key path — that tool refuses, and
        // sending an operator to it wastes the trip. The `Corrupt` arm also
        // single-quotes the path, or a copy-paste with a space in it fails
        // with "unknown argument".
        let dir = tempfile::TempDir::new().unwrap();

        let corrupt = keygen_requirement(dir.path(), KeyCacheState::Corrupt {
            why: "event_pk.bin digest does not match the manifest".into(),
        })
        .expect_err("a cache whose records disagree with its contents is not one to burn on");
        let msg = format!("{corrupt}");
        assert!(msg.contains("--repair"), "must hand over the tool: {msg}");
        assert!(
            msg.contains("digest does not match"),
            "must keep `why`: {msg}"
        );
        assert!(msg.contains("'"), "must quote the path: {msg}");

        let blocked = keygen_requirement(dir.path(), KeyCacheState::Blocked {
            why: "event_vk.bin is a directory".into(),
        })
        .expect_err("a directory on a key path is a refusal");
        let msg = format!("{blocked}");
        assert!(
            !msg.contains("--repair"),
            "--repair refuses this state; offering it sends the operator nowhere: {msg}"
        );
        assert!(
            !msg.contains("Clearing it is safe"),
            "clearing is exactly what does not work here: {msg}"
        );
        assert!(msg.contains("is a directory"), "must keep `why`: {msg}");
    }

    #[test]
    fn a_client_context_that_cannot_be_built_is_a_pre_send_refusal() {
        // Exit 2 is published as "refused before sending, nothing left the
        // machine", and that is what this is: `ClientContext::new` only
        // builds config — it does not connect — so every failure of it is
        // local and happens before any message is composed. The
        // orchestrator kept a byte-identical copy of this function that
        // said exit 10 instead, which README's table defines as "the USDC
        // has left the source multisig regardless".
        for endpoint in ["", " ", "not a url", "http://", "://x"] {
            let err = build_client_context(endpoint)
                .expect_err("a malformed endpoint cannot build a context");
            assert_eq!(
                err.exit_code().as_i32(),
                2,
                "{endpoint:?} is refused before anything is sent: {err}",
            );
            assert_ne!(
                err.exit_code().as_i32(),
                10,
                "{endpoint:?}: exit 10 claims a burn that was never composed",
            );
        }
    }

    #[test]
    fn the_client_context_is_built_in_exactly_one_place() {
        // Two copies is how the exit codes came to disagree: the same
        // construction, the same pre-send moment, refused as exit 2 here
        // and reported as exit 10 in the orchestrator. Constructing one
        // somewhere new means deciding again what its failure means, so
        // make that decision visible rather than inherited.
        // The only guard in the crate that cuts files it does not own,
        // and the last one still cutting at `#[cfg(test)]` alone — an
        // attribute on any item above a test module truncated the scan of
        // somebody else's file, and a second constructor below the cut
        // then counted as zero. Fail-open, in the direction where the
        // exit codes go back to disagreeing about whether a burn was
        // composed. `production_source` cuts at the test module itself
        // and panics rather than widening if it cannot find one.
        let needle = concat!("ClientContext::", "new(");
        let count = |file: &str, src: &str| {
            production_source(file, src)
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| l.contains(needle))
                .count()
        };
        let here = count("preflight.rs", include_str!("preflight.rs"));
        let there = count("orchestrator.rs", include_str!("orchestrator.rs"))
            + count("burn.rs", include_str!("burn.rs"));
        assert_eq!(here, 1, "this module owns the constructor");
        assert_eq!(
            there, 0,
            "the withdraw pipeline shares that constructor; a second one has to choose an exit \
             code for its failure, and the copy that chose 10 reported a burn nobody had composed",
        );
    }

    #[test]
    fn the_headroom_check_reads_both_device_ids() {
        // The composition, reached at last. `space_requirements` and
        // `share_a_filesystem` were each pinned for two rounds while
        // nothing called the function that puts them together — an
        // unconditional `panic!` as its first statement left the whole
        // suite green — so replacing `shared` with a constant `false`
        // passed too, and quietly halved what the run reserves on the
        // shipped profile, where the pk cache lives INSIDE params.
        let d = tempfile::TempDir::new().unwrap();
        let params = d.path().join("params");
        let pk_cache = params.join("pk_cache");
        std::fs::create_dir_all(&pk_cache).unwrap();

        let plan = plan_disk_headroom(&params, &pk_cache, KeyCacheState::Cold {
            why: "no manifest".into(),
        })
        .expect("a cold cache is a requirement, not a refusal");

        assert_eq!(
            plan.len(),
            1,
            "two directories of one tempdir share a device, so the run must ask that device once \
             for the total: {plan:?}",
        );
        assert_eq!(
            plan[0].needed,
            EVENT_KEYGEN_BYTES + OUTER_PK_BYTES,
            "and the total is the sum — the shortfall this exists for lands mid-aggregation, \
             after the burn",
        );
    }

    #[test]
    fn a_cache_that_cannot_be_trusted_refuses_before_any_measuring() {
        // The other half of "is this function reached at all": a verdict
        // that is a refusal rather than a number must come back out of
        // `check_disk_headroom` itself, not merely out of the helper.
        // This is the one deterministic path through the whole function —
        // it needs no assumption about the host's free space.
        let d = tempfile::TempDir::new().unwrap();
        let err = check_disk_headroom(d.path(), d.path(), KeyCacheState::Corrupt {
            why: "digest mismatch".into(),
        })
        .expect_err("an untrustworthy cache is not something to spend a burn on");
        assert_eq!(err.exit_code().as_i32(), 2, "{err}");
        let msg = format!("{err}");
        assert!(msg.contains("digest mismatch"), "must keep `why`: {msg}");
        assert!(msg.contains("--repair"), "and name the way out: {msg}");
    }

    #[test]
    fn a_leaked_keygen_temp_is_reported_before_the_measurement_decides_anything() {
        // A killed keygen leaves its whole partial temp behind
        // (`NamedTempFile` cleans up on drop, not on SIGKILL), the name is
        // a dotfile so `ls` does not show it, and it is holding space this
        // run may need. Reporting it only when the measurement then
        // refuses would hide it in exactly the case where the run
        // continues and the next one does not.
        //
        // Captured rather than eyeballed: `with_default` is per-thread, so
        // this does not disturb tests running beside it.
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<String>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(buf));
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
            type Writer = Sink;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let d = tempfile::TempDir::new().unwrap();
        // `tempfile`'s own shape: `.tmp` plus six alphanumerics.
        let leak = d.path().join(".tmpAb12Cd");
        std::fs::write(&leak, vec![0u8; 4096]).unwrap();

        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .finish();
        // Bound, not discarded: `with_default` hands back what the closure
        // returned, and saying which refusal it was documents the fixture.
        let outcome = tracing::subscriber::with_default(subscriber, || {
            // `Corrupt` so the outcome is a refusal on every host: the
            // point here is what was said on the way, not the verdict.
            check_disk_headroom(d.path(), d.path(), KeyCacheState::Corrupt {
                why: "digest mismatch".into(),
            })
        });
        outcome.expect_err("a corrupt cache is a refusal");

        let logged = sink.0.lock().unwrap().clone();
        assert!(
            logged.contains(".tmpAb12Cd"),
            "the leak must be named before the measurement decides anything: {logged}",
        );
        assert!(
            logged.contains("--repair"),
            "and it must say how to remove it: {logged}",
        );
    }

    #[test]
    fn two_directories_on_one_filesystem_are_asked_for_the_sum() {
        // The composition the function exists for, and the one thing the
        // suite could not see before: `require_space` and both helpers
        // were covered, the summing was not. Two independent comparisons
        // against the same statvfs both pass at 3.5 GB free while the run
        // needs ~3.65 GB, and the shortfall then lands during aggregation
        // — after the burn.
        let plan = space_requirements(
            Path::new("/params"),
            EVENT_KEYGEN_BYTES,
            Path::new("/params/pk_cache"),
            OUTER_PK_BYTES,
            true,
        );
        assert_eq!(plan.len(), 1, "one filesystem, one comparison: {plan:?}");
        assert_eq!(plan[0].probe, Path::new("/params"));
        assert_eq!(
            plan[0].needed,
            EVENT_KEYGEN_BYTES + OUTER_PK_BYTES,
            "the total is the sum; either half alone is what let the run start and fail late",
        );
        assert_eq!(plan[0].parts.as_slice(), &[
            ("--params-dir", EVENT_KEYGEN_BYTES),
            ("--pk-cache-dir", OUTER_PK_BYTES),
        ]);
    }

    #[test]
    fn two_filesystems_are_asked_separately() {
        // The other half of the composition: separate pools must not be
        // charged for each other, or a host with a small params volume and
        // a large cache volume is refused a run that fits.
        let plan = space_requirements(
            Path::new("/params"),
            EVENT_KEYGEN_BYTES,
            Path::new("/mnt/other/pk_cache"),
            OUTER_PK_BYTES,
            false,
        );
        assert_eq!(plan.len(), 2, "two pools, two comparisons: {plan:?}");
        assert_eq!(plan[0].probe, Path::new("/params"));
        assert_eq!(plan[0].needed, EVENT_KEYGEN_BYTES);
        assert_eq!(plan[1].probe, Path::new("/mnt/other/pk_cache"));
        assert_eq!(plan[1].needed, OUTER_PK_BYTES);
        assert!(
            plan.iter()
                .all(|r| r.parts.len() == 1 && r.parts[0].1 == r.needed),
            "neither requirement may carry the other's bytes: {plan:?}",
        );
    }

    #[test]
    fn a_device_id_that_cannot_be_read_is_assumed_shared() {
        // The arm nothing could reach. It was inlined in the match, so
        // flipping it to `false` left the whole suite green while halving
        // what the run reserves on every host whose `st_dev` we cannot
        // read — and unreadable is the normal case for a `--pk-cache-dir`
        // that does not exist yet.
        assert!(share_a_filesystem(None, Some(7)), "params device unknown");
        assert!(share_a_filesystem(Some(7), None), "pk_cache device unknown");
        assert!(share_a_filesystem(None, None), "neither readable");
        // And the two it can read.
        assert!(share_a_filesystem(Some(7), Some(7)), "same st_dev");
        assert!(!share_a_filesystem(Some(7), Some(8)), "different st_dev");
    }

    #[test]
    fn zero_required_never_touches_the_filesystem() {
        // The warm-cache path reaches `require_space` with 0, and a path
        // that does not exist must not turn that into a refusal.
        require_space(Path::new("/nonexistent/for/this/test"), 0, &[])
            .expect("nothing to write means nothing to check");
    }

    #[test]
    fn an_unmeasurable_filesystem_is_not_a_refusal() {
        // statvfs failing is a failed measurement, not evidence of a full
        // disk. Inventing a refusal out of it would block runs on unusual
        // mounts for no reason.
        require_space(
            Path::new("/nonexistent/for/this/test"),
            EVENT_KEYGEN_BYTES,
            &[("--params-dir", EVENT_KEYGEN_BYTES)],
        )
        .expect("a failed statvfs must not be read as 'no space'");
    }

    #[test]
    fn a_shortfall_itemises_what_shares_the_filesystem() {
        // "needs 4.0 GB" invites the operator to free 4 GB and be
        // surprised. The sum is the whole reason this is one check.
        let dir = tempfile::TempDir::new().unwrap();
        let absurd = u64::MAX / 4; // ~4.6 exabytes: no test host has this
        let err = require_space(dir.path(), absurd, &[
            ("--params-dir", EVENT_KEYGEN_BYTES),
            ("--pk-cache-dir", OUTER_PK_BYTES),
        ])
        .expect_err("no filesystem in this test has exabytes free");
        let msg = format!("{err}");
        assert!(
            msg.contains("--params-dir 3.2 GB"),
            "must itemise keygen: {msg}"
        );
        assert!(
            msg.contains("--pk-cache-dir 1.1 GB"),
            "must itemise the aggregator: {msg}"
        );
        assert!(
            msg.contains("share one filesystem"),
            "must say why they add up: {msg}"
        );
    }

    #[test]
    fn a_shortfall_names_the_space_a_dead_keygen_is_holding() {
        // "Free 4.0 GB" is the wrong instruction when most of the
        // shortfall is a temp file the operator cannot see: the name
        // starts with a dot, so `ls` does not show it, and a killed
        // keygen is the only thing that produces one.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".tmpAbC123"), vec![0u8; 4096]).unwrap();
        let err = require_space(dir.path(), u64::MAX / 4, &[(
            "--params-dir",
            EVENT_KEYGEN_BYTES,
        )])
        .expect_err("no filesystem in this test has exabytes free");
        let msg = format!("{err}");
        assert!(
            msg.contains("interrupted-keygen temp file"),
            "must say what is holding the space: {msg}"
        );
        assert!(
            msg.contains("probe_event_keys"),
            "must name the tool: {msg}"
        );
        assert!(msg.contains("--repair"), "and the flag: {msg}");
        assert!(
            msg.contains("hidden"),
            "must say why the operator has not seen it: {msg}"
        );
    }

    #[test]
    fn a_shortfall_with_no_litter_does_not_invent_any() {
        // The other half: a host that is simply full must not be told to
        // go hunting for temp files that are not there.
        let dir = tempfile::TempDir::new().unwrap();
        let err = require_space(dir.path(), u64::MAX / 4, &[(
            "--params-dir",
            EVENT_KEYGEN_BYTES,
        )])
        .expect_err("no filesystem in this test has exabytes free");
        let msg = format!("{err}");
        assert!(
            !msg.contains("interrupted-keygen"),
            "nothing leaked here: {msg}"
        );
    }

    #[test]
    fn a_zero_part_is_not_itemised() {
        // A warm cache contributes 0. Printing "--params-dir 0.0 GB" would
        // point the operator at the one thing that needs nothing.
        let dir = tempfile::TempDir::new().unwrap();
        let err = require_space(dir.path(), u64::MAX / 4, &[
            ("--params-dir", 0),
            ("--pk-cache-dir", OUTER_PK_BYTES),
        ])
        .expect_err("no filesystem in this test has exabytes free");
        let msg = format!("{err}");
        assert!(
            !msg.contains("--params-dir"),
            "0 bytes is not a part: {msg}"
        );
        assert!(msg.contains("--pk-cache-dir"), "got: {msg}");
    }

    #[test]
    fn two_directories_under_one_root_are_one_device() {
        // `st_dev`, not path comparison: a bind mount or a symlink makes
        // two unrelated-looking paths share a pool, and two separate checks
        // against the same statvfs both pass while the run needs the sum.
        let dir = tempfile::TempDir::new().unwrap();
        let a = dir.path().join("params");
        let b = dir.path().join("params/pk_cache");
        std::fs::create_dir_all(&b).unwrap();
        assert_eq!(device_of(&a), device_of(&b));
        assert!(device_of(&a).is_some());
        // And a path that is not there is not a device.
        assert_eq!(device_of(Path::new("/nonexistent/for/this/test")), None);
    }

    #[test]
    fn free_space_is_measurable_here_and_not_elsewhere() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(
            available_bytes(dir.path()).is_some_and(|b| b > 0),
            "a writable tempdir has measurable free space"
        );
        assert_eq!(
            available_bytes(Path::new("/nonexistent/for/this/test")),
            None
        );
    }

    #[test]
    fn a_warm_cache_still_reserves_for_the_aggregator() {
        // The pk-cache requirement is unconditional — its slot name is a
        // hash over a proof that does not exist until stage 5, so there is
        // nothing on disk to narrow it against.
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(pk_cache_requirement(dir.path()), OUTER_PK_BYTES);
        assert_eq!(
            pk_cache_requirement(Path::new("/nonexistent")),
            OUTER_PK_BYTES
        );
    }

    #[tokio::test]
    async fn a_correct_deploy_passes() {
        // Nothing proves a refusal is a refusal until something proves the
        // check can also return Ok. Without this test an inverted
        // comparison anywhere above turns every correct deploy into a
        // refusal, and all four negative tests still pass.
        let url = mock_rpc_code_for(&full_walk_code(), full_walk(&[])).await;
        check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect("a deploy that matches on every check must be accepted");
    }

    #[tokio::test]
    async fn a_deployed_verifier_from_another_circuit_is_refused() {
        // The whole deployed-bytecode arm, which nothing reached before:
        // `verifiers_dir` was `None` in every test, so the branch that
        // compares the committed .bin against `eth_getCode(yul)` never ran.
        // Its failure lands in stage 6 otherwise — after the burn, after the
        // ~91 min anchor wait, and after the proof.
        let dir = tempfile::TempDir::new().unwrap();
        // 32-byte CREATE prelude + a runtime that is NOT what the chain has.
        let mut bin = vec![0u8; 32];
        bin.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        std::fs::write(dir.path().join(WITHDRAW_VERIFIER_BIN), &bin).unwrap();

        // The mock serves SOME_CODE ("0x60806040") at address 4.
        let url = mock_rpc_code_for(&full_walk_code(), full_walk(&[])).await;
        let err = check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            Some(dir.path()),
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("a verifier from another circuit rejects every proof this build makes");
        let msg = format!("{err}");
        assert!(
            msg.contains(WITHDRAW_VERIFIER_BIN),
            "must name the artefact: {msg}"
        );
        assert!(
            msg.contains("after the burn"),
            "must say when the failure would otherwise land: {msg}"
        );
    }

    #[tokio::test]
    async fn a_matching_deployed_verifier_passes() {
        // The other half: the comparison must accept the bytes it is given.
        // SOME_CODE is "0x60806040" = the four bytes below.
        let dir = tempfile::TempDir::new().unwrap();
        let mut bin = vec![0u8; 32];
        bin.extend_from_slice(&[0x60, 0x80, 0x60, 0x40]);
        std::fs::write(dir.path().join(WITHDRAW_VERIFIER_BIN), &bin).unwrap();

        let url = mock_rpc_code_for(&full_walk_code(), full_walk(&[])).await;
        check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            Some(dir.path()),
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect("the deployed runtime equals the committed one past its CREATE prelude");
    }

    #[tokio::test]
    async fn a_treasury_shortfall_is_refused_before_the_burn() {
        // `treasuryBalance` is a new ABI binding no test called. Its check
        // is the last of the five, so this also proves the four before it
        // are reachable in sequence.
        let short = format!("0x{:0>64x}", 999_999u64); // one µUSDC short
        let url = mock_rpc_code_for(&full_walk_code(), full_walk(&[(SEL_TREASURY, short)])).await;
        let err = check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect_err("withdrawByProof would revert WithdrawTreasuryShortfall");
        let msg = format!("{err}");
        assert!(
            msg.contains("WithdrawTreasuryShortfall"),
            "must name the revert: {msg}"
        );
        assert!(msg.contains("999999"), "must show what is there: {msg}");
        assert!(msg.contains("1000000"), "must show what is needed: {msg}");
        assert!(
            msg.contains("not a guarantee"),
            "must not promise the treasury will still be there: {msg}"
        );
    }

    #[tokio::test]
    async fn exactly_enough_treasury_is_not_a_shortfall() {
        // The boundary the `<` sits on. `<=` here would refuse every
        // withdrawal that drains the treasury exactly.
        let exact = format!("0x{:0>64x}", 1_000_000u64);
        let url = mock_rpc_code_for(&full_walk_code(), full_walk(&[(SEL_TREASURY, exact)])).await;
        check_bridge_deploy(
            &url,
            Address::repeat_byte(1),
            None,
            (U256::ZERO, U256::ZERO),
            &UsdcAmount(1_000_000),
        )
        .await
        .expect("a treasury holding exactly the withdrawal amount is enough");
    }

    #[tokio::test]
    async fn the_wrong_destination_chain_is_refused() {
        // The mock answers eth_chainId 0xaa36a7 = 11155111 (Sepolia). A
        // withdrawal aimed at the wrong chain cannot be undone, so this
        // refusal has to happen before the burn — and nothing tested that
        // it happens at all.
        let url = mock_rpc(SOME_CODE, Default::default()).await;
        let err = check_destination_chain(&url, 1)
            .await
            .expect_err("--rpc-url on Sepolia with --to-chain 1 must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("11155111"),
            "must name what the node reports: {msg}"
        );
        assert!(
            msg.contains("cannot be undone"),
            "must say why it refuses: {msg}"
        );
    }

    #[tokio::test]
    async fn the_right_destination_chain_passes() {
        let url = mock_rpc(SOME_CODE, Default::default()).await;
        check_destination_chain(&url, 11155111)
            .await
            .expect("--to-chain matching the node must be accepted");
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
        // The refusal ends by sending the operator to USDC_BRIDGE_ACCOUNT_ID,
        // which is 64 lowercase hex characters in the profile. `U256`'s
        // `Display` is decimal, so the shipped message printed
        // `11806252235961651298089590628336806290921645495320214372650577192963691649562`
        // where the operator had to compare `1a1a…1a1a` — the one rendering
        // that makes the instruction impossible to follow. Both sides are
        // asserted: the hex must be there AND the decimal must not, because
        // adding hex somewhere else in the sentence would satisfy the first
        // alone.
        assert!(
            msg.contains(&format!("({:064x}, {:064x})", U256::ZERO, U256::ZERO)),
            "the on-chain pair must be 64-hex, comparable to the profile line, got: {msg}"
        );
        assert!(
            msg.contains(&format!(
                "({:064x}, {:064x})",
                U256::from(7u8),
                U256::from(9u8)
            )),
            "the proof's own pair must be 64-hex too, got: {msg}"
        );
        assert!(
            !msg.contains("(0, 0)") && !msg.contains("(7, 9)"),
            "decimal cannot be compared against USDC_BRIDGE_ACCOUNT_ID by eye, got: {msg}"
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
        assert!(
            msg.contains("--from-keys"),
            "must name the flag, got: {msg}"
        );
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
        // The whole point of the ticket's "a valid pair" wording. Two
        // independently well-formed 64-hex strings are NOT a key pair, and
        // letting them through means the multisig rejects the signature after
        // the message is already on the wire — an ambiguous exit 10 instead of
        // a clean exit 2.
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_keys(dir.path(), &"a".repeat(64), &"1".repeat(64));

        let err = load_owner_keypair_hex(&path)
            .expect_err("independent public/secret halves must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("--from-keys"),
            "must name the flag, got: {msg}"
        );
        assert!(
            msg.contains("do not form a key pair") || msg.contains("does not match"),
            "must say why, got: {msg}",
        );
        assert!(
            !msg.contains(&"a".repeat(64)) && !msg.contains(&"1".repeat(64)),
            "must not echo any half of the key file, got: {msg}",
        );
    }

    // -- prover artifacts -------------------------------------------------

    /// Build an artifact tree that satisfies everything except what the
    /// caller removes afterwards. `params/` is left empty on purpose: no
    /// test here goes through `check_prover_artifacts`, precisely because
    /// that would short-circuit on the ceremony and no later assertion would
    /// ever run. Each check is exercised directly.
    fn make_artifact_tree(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("agg/target/release")).unwrap();
        std::fs::write(root.join("agg/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::create_dir_all(root.join("agg/src/bin")).unwrap();
        std::fs::write(
            root.join("agg/src/bin/aggregate_proof.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("ver")).unwrap();
        // The real bytes. Anything else is not a verifier, and the check now
        // says so — a fixture of plausibly-sized zeros would only prove that
        // the check is too weak to notice.
        std::fs::write(
            root.join("ver/BridgeWithdrawalAggregatorVerifier.bin"),
            EXPECTED_VERIFIER_BIN,
        )
        .unwrap();
        std::fs::write(
            root.join("ver/BridgeWithdrawalAggregatorVerifier.sol"),
            EXPECTED_VERIFIER_SOL,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("params")).unwrap();
        std::fs::create_dir_all(root.join("work")).unwrap();
    }

    /// Write a shell script at `path` and make it executable. Stands in for
    /// the real `aggregate-proof`, whose `--help` prints the usage line
    /// these fixtures imitate.
    fn write_exec(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn ceremony_check_names_the_provisioning_commands_when_params_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());

        let err = check_ceremony(&dir.path().join("params"))
            .expect_err("an empty params dir must refuse before the burn");
        let msg = format!("{err}");
        assert!(
            msg.contains("kzg_bn254_21.srs"),
            "must name the file, got: {msg}"
        );
        assert!(
            msg.contains("bootstrap_hermez_srs"),
            "must name the tool, got: {msg}"
        );
        assert!(
            msg.contains("powersOfTau28_hez_final_21.ptau"),
            "must name the ptau, got: {msg}"
        );
        // The shell script may be NAMED — the message disambiguates the two
        // tools on purpose, because an operator who reaches for the wrong
        // one gets K=20 in the wrong directory and no useful error. What it
        // must never do is OFFER it as the remedy. So the assertion is about
        // context, not about the substring: every line that mentions the
        // script must be the line that says it is the wrong tool.
        //
        // A bare `!msg.contains(...)` would fail this message and would have
        // to be satisfied by deleting the disambiguation, which is the one
        // sentence that keeps an operator off the wrong path.
        for line in msg
            .lines()
            .filter(|l| l.contains("bootstrap_hermez_srs.sh"))
        {
            assert!(
                line.contains("different tool"),
                "the K=20/bridge-snark-utils shell script may only be named to warn against it,                  never offered as the remedy; got the line: {line}"
            );
        }
        assert!(
            msg.contains("--bin bootstrap_hermez_srs"),
            "the remedy must be the crate's own binary, got: {msg}"
        );
    }

    #[test]
    fn verifier_check_refuses_a_dir_without_the_committed_bin() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        std::fs::remove_file(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.bin"),
        )
        .unwrap();

        let err = check_verifier_bin(&dir.path().join("ver"), false).expect_err(
            "stage 1 compares this file with the deployed chain runtime; its absence is fatal",
        );
        assert!(
            format!("{err}").contains("BridgeWithdrawalAggregatorVerifier.bin"),
            "got: {err}"
        );
    }

    #[test]
    fn verifier_check_refuses_a_truncated_bin() {
        // The realistic corruption: an interrupted checkout or an LFS miss
        // leaves a short file.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        std::fs::write(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.bin"),
            b"\x00",
        )
        .unwrap();

        let err = check_verifier_bin(&dir.path().join("ver"), false)
            .expect_err("a 1-byte verifier must be refused");
        assert!(
            format!("{err}").contains("sha256"),
            "must name the mismatch, got: {err}"
        );
    }

    #[test]
    fn verifier_check_refuses_the_right_size_with_wrong_contents() {
        // The case a size range waves through, and the reason this check
        // compares bytes: a stale verifier from a previous rotation is
        // exactly the right size and completely wrong.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let mut corrupted = EXPECTED_VERIFIER_BIN.to_vec();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xff;
        std::fs::write(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.bin"),
            &corrupted,
        )
        .unwrap();

        let err = check_verifier_bin(&dir.path().join("ver"), false)
            .expect_err("one flipped byte must be refused before the burn");
        let msg = format!("{err}");
        assert!(msg.contains("sha256"), "got: {msg}");
        assert!(
            msg.contains("--allow-verifier-drift"),
            "self-deploy users need the escape hatch named, got: {msg}",
        );
    }

    #[test]
    fn verifier_check_accepts_the_expected_bin() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        check_verifier_bin(&dir.path().join("ver"), false)
            .expect("the embedded verifier must match");
    }

    #[test]
    fn source_check_refuses_a_dir_without_the_committed_sol() {
        // aggregate-proof compares its regenerated source with this file at
        // stage 5, after the burn; without it the run cannot finish.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        std::fs::remove_file(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.sol"),
        )
        .unwrap();

        let err = check_verifier_sol(&dir.path().join("ver"), false)
            .expect_err("a missing source must be refused before the burn");
        assert!(
            format!("{err}").contains("BridgeWithdrawalAggregatorVerifier.sol"),
            "got: {err}"
        );
    }

    #[test]
    fn source_check_refuses_a_stale_source_and_names_the_escape_hatch() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let mut stale = EXPECTED_VERIFIER_SOL.to_vec();
        stale.extend_from_slice(b"// from another rotation\n");
        std::fs::write(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.sol"),
            &stale,
        )
        .unwrap();

        let err = check_verifier_sol(&dir.path().join("ver"), false)
            .expect_err("a source that is not this build's must be refused");
        let msg = format!("{err}");
        assert!(msg.contains("sha256"), "got: {msg}");
        assert!(
            msg.contains("--allow-verifier-drift"),
            "self-deploy users need the escape hatch named, got: {msg}",
        );
    }

    #[test]
    fn source_check_accepts_the_expected_sol() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        check_verifier_sol(&dir.path().join("ver"), false).expect("the embedded source must match");
    }

    #[test]
    fn source_check_lets_a_self_deploy_through_with_the_flag() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        std::fs::write(
            dir.path()
                .join("ver/BridgeWithdrawalAggregatorVerifier.sol"),
            b"// a self-deployed verifier\n",
        )
        .unwrap();
        check_verifier_sol(&dir.path().join("ver"), true)
            .expect("--allow-verifier-drift covers the source as well as the bytecode");
    }

    #[tokio::test]
    async fn aggregator_check_refuses_a_missing_release_binary() {
        // A present crate is not a verified aggregator. Preflight refuses
        // and names the build command rather than trusting the runtime's
        // cargo fallback, which it cannot check without paying for a cold
        // build.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path()); // crate sources present, no binary

        let err = check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect_err("preflight requires a prebuilt aggregate-proof");
        let msg = format!("{err}");
        assert!(
            msg.contains("cargo build --release"),
            "must name the fix, got: {msg}"
        );
    }

    #[tokio::test]
    async fn aggregator_check_refuses_a_present_but_unrunnable_binary() {
        // The case existence-only checking waves through, and the one that
        // hurts most: SubprocessAggregator picks this file over the working
        // cargo fallback, so a stale or wrong-arch build is *preferred* and
        // then dies after the burn.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let bin = dir.path().join("agg/target/release/aggregate-proof");
        std::fs::write(&bin, b"not an executable").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect_err(
                "a non-executable aggregate-proof must be refused, not accepted on is_file()",
            );
        let msg = format!("{err}");
        assert!(msg.contains("aggregate-proof"), "got: {msg}");
        assert!(
            msg.contains("cargo build --release"),
            "must tell the operator how to recover, and must not suggest the cargo-run fallback \
             preflight itself forbids, got: {msg}",
        );
        assert!(
            !msg.contains("cargo run"),
            "must not point at a fallback this check refuses, got: {msg}",
        );
    }

    #[tokio::test]
    async fn aggregator_check_refuses_a_binary_that_exits_zero_but_is_not_ours() {
        // `exit 0` is not evidence. Some unrelated tool parked at the
        // expected path — or a stub someone dropped there to get past a
        // check — would pass a bare exit-status test and then fail on the
        // real invocation, after the burn.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        write_exec(
            &dir.path().join("agg/target/release/aggregate-proof"),
            "#!/bin/sh\nexit 0\n",
        );

        let err = check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect_err("exit 0 alone must not count as a working aggregate-proof");
        assert!(format!("{err}").contains("--inner-snark"), "got: {err}");
    }

    #[tokio::test]
    async fn aggregator_check_kills_a_hanging_binary() {
        // `timeout` bounds how long WE wait; without `kill_on_drop` the
        // child outlives the cancelled future and keeps running. The
        // assertion is that the probe returns AND the process is gone.
        //
        // Two things this test must NOT do, both of which look natural:
        //
        //  - trap SIGTERM in the script and check for a marker file. `kill_on_drop`
        //    calls `Child::start_kill`, which on Unix is SIGKILL, so a TERM handler
        //    never runs and that test would fail whether or not the fix is present.
        //  - wait out the production timeout. Thirty seconds in a unit suite is a tax
        //    on every future run, so the probe takes its timeout as a parameter and the
        //    test passes a short one.
        //
        // So: have the child publish its pid and then `exec`, and check the
        // pid is gone afterwards. `exec` matters — without it `sh` forks
        // `sleep` and only the shell is killed.
        let dir = tempfile::TempDir::new().unwrap();
        let bin_dir = dir.path().join("target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pidfile = dir.path().join("child.pid");
        // Write to a temp name and rename, so the file is never observed
        // half-written: the handshake below reads it as soon as it appears.
        write_exec(
            &bin_dir.join("aggregate-proof"),
            &format!(
                "#!/bin/sh\necho $$ > {p}.tmp\nmv {p}.tmp {p}\nexec sleep 120\n",
                p = pidfile.display()
            ),
        );

        // The child is spawned BY the probe, so the readiness handshake can
        // only happen after the probe's clock has started. The race is
        // therefore structural and the only defence is to make the two
        // bounds provably non-overlapping: the pid must be recorded strictly
        // before the timeout can fire.
        const HANDSHAKE: Duration = Duration::from_secs(5);
        const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
        // Stated as an assertion rather than a comment, so a later tidy-up
        // that shortens PROBE_TIMEOUT fails loudly instead of going flaky.
        assert!(
            HANDSHAKE.saturating_mul(2) <= PROBE_TIMEOUT,
            "the handshake must complete with room to spare before the probe times out"
        );

        let probe = tokio::spawn({
            let dir = dir.path().to_path_buf();
            async move { check_aggregator_runnable_with_timeout(&dir, PROBE_TIMEOUT).await }
        });

        let deadline = std::time::Instant::now() + HANDSHAKE;
        while !pidfile.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            pidfile.exists(),
            "the fake aggregator did not record its pid within {HANDSHAKE:?} — the runner could \
             not start a shell in that time, which is a problem with the runner rather than with \
             kill_on_drop"
        );

        let started = std::time::Instant::now();
        let err = probe
            .await
            .expect("the probe task must not itself panic")
            .expect_err("a binary that never answers --help must be refused");
        assert!(
            format!("{err}").contains("did not respond"),
            "the refusal must name the timeout, got: {err}"
        );
        assert!(
            started.elapsed() < PROBE_TIMEOUT + Duration::from_secs(5),
            "the probe must return at its timeout, not wait out the child's 120 s"
        );

        // SIGKILL plus reaping is not instantaneous; poll briefly rather
        // than sleeping a fixed amount and hoping.
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("the child must have started")
            .trim()
            .parse()
            .unwrap();
        let mut gone = false;
        for _ in 0..50 {
            // signal 0 tests for existence without sending anything.
            if unsafe { libc::kill(pid, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            gone,
            "pid {pid} survived the timeout — kill_on_drop is missing, and a hung aggregator \
             would outlive every withdrawal that probed it"
        );
    }

    #[tokio::test]
    async fn aggregator_check_accepts_a_relative_aggregator_dir() {
        // The shipped profile's `BRIDGE_AGGREGATOR_DIR=../bridge-evm-aggregator`
        // is relative, so this is the default path, not an edge case.
        // Without the canonicalize the child resolves the program against
        // its own cwd — which `current_dir` has just set to the same
        // relative directory — and execs
        // `<dir>/<dir>/target/release/aggregate-proof`.
        //
        // Create the fixture INSIDE the process cwd so a relative path is a
        // plain file name — no path-arithmetic crate, and no assumption
        // about where the temp dir lives relative to the test binary.
        let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
        let bin_dir = dir.path().join("target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();
        write_exec(
            &bin_dir.join("aggregate-proof"),
            "#!/bin/sh\necho 'aggregate-proof --inner-snark <path> --name <verifier> \
             --allow-source-drift'\n",
        );

        let relative = std::path::Path::new(dir.path().file_name().unwrap());
        assert!(
            relative.is_relative(),
            "the fixture must actually be relative"
        );

        check_aggregator_runnable(relative)
            .await
            .expect("a relative --aggregator-dir must work; the shipped profile uses one");
    }

    #[tokio::test]
    async fn aggregator_check_accepts_a_binary_whose_help_names_our_flags() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        write_exec(
            &dir.path().join("agg/target/release/aggregate-proof"),
            "#!/bin/sh\necho 'usage: aggregate-proof --inner-snark <path> --name <n> \
             --allow-source-drift'\nexit 0\n",
        );

        check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect("a binary that answers --help with our flags must pass");
    }

    #[tokio::test]
    async fn aggregator_check_refuses_a_binary_that_predates_the_source_self_check() {
        // The old binary's --help still names --inner-snark, so the plain
        // flag-name probe alone would wave it through — straight past the
        // burn and the anchor wait into a stage-5 panic with no `solc` on
        // PATH. `--allow-source-drift` only exists in the new usage line, so
        // its absence in an otherwise-recognised --help is what must trip
        // this refusal, with a message distinct from "not aggregate-proof at
        // all".
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        write_exec(
            &dir.path().join("agg/target/release/aggregate-proof"),
            "#!/bin/sh\ncat <<'USAGE'\naggregate-proof --inner-snark <path> --name <verifier> \
             --out <path>\n [--verifiers-dir <dir>] [--k-outer <n>] [--universality <mode>]\n \
             [--allow-bin-drift] [--pk-cache-dir <dir>]\nUSAGE\n",
        );

        let err = check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect_err(
                "a pre-self-check aggregate-proof must be refused, not run into a solc-less stage \
                 5 after the burn",
            );
        let msg = format!("{err}");
        assert!(
            msg.contains("solc"),
            "must explain that the old binary still needs solc, got: {msg}"
        );
        assert!(
            msg.contains("cargo build --release --bin aggregate-proof"),
            "must name the rebuild command, got: {msg}"
        );
    }

    #[tokio::test]
    async fn aggregator_check_accepts_a_binary_whose_help_names_the_source_self_check() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        write_exec(
            &dir.path().join("agg/target/release/aggregate-proof"),
            "#!/bin/sh\ncat <<'USAGE'\naggregate-proof --inner-snark <path> --name <verifier> \
             --out <path>\n [--verifiers-dir <dir>] [--k-outer <n>] [--universality <mode>]\n \
             [--allow-source-drift] [--pk-cache-dir <dir>]\nUSAGE\n",
        );

        check_aggregator_runnable(&dir.path().join("agg"))
            .await
            .expect("a --help naming --allow-source-drift must pass");
    }

    #[test]
    fn output_dirs_check_refuses_an_unusable_path() {
        // Deliberately NOT a chmod 0500 directory: CI runs these jobs as
        // root in `rustlang/rust:nightly` (`.gitlab-ci.yml` sets no `user:`),
        // and root ignores the write bit — the test would pass locally and
        // fail in CI for a reason unrelated to the code. A path whose parent
        // is a regular file fails with ENOTDIR for every uid.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let work = blocker.join("work");

        let err = check_output_dirs(
            &work,
            &dir.path().join("snark"),
            // Resolved by the caller, so a plain `&Path` here — not
            // `Option`.
            &dir.path().join("params/pk_cache"),
            Some(&dir.path().join("params")),
        )
        .expect_err("an unusable work_dir path fails at stage 5, after the burn");
        assert!(format!("{err}").contains("--work-dir"), "got: {err}");
    }

    #[test]
    fn output_dirs_check_requires_a_writable_params_dir() {
        // The first Circuit-4 keygen writes event_{vk,pk}.bin into
        // params_dir itself, and every shipped profile sets
        // BRIDGE_PK_CACHE_DIR, so the params_dir/pk_cache default never
        // covers it.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let blocker = dir.path().join("params-is-a-file");
        std::fs::write(&blocker, b"x").unwrap();

        let err = check_output_dirs(
            &dir.path().join("work"),
            &dir.path().join("snark"),
            &dir.path().join("pkcache"), // explicit, as the profiles set it
            // `Some(..)` = this run will write keys here, i.e. a cold cache.
            // `None` would mean warm, and a warm run is allowed a read-only
            // params dir — so passing `None` here would test nothing.
            Some(&blocker.join("params")),
        )
        .expect_err("an unusable params_dir must refuse — keygen writes into it");
        assert!(format!("{err}").contains("--params-dir"), "got: {err}");
    }

    #[test]
    fn a_warm_cache_does_not_need_a_writable_params_dir() {
        // The read-only / shared `params/` the config file contemplates.
        // With `None` for params_dir — what a warm probe produces — an
        // unwritable params dir is simply not this run's problem.
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let blocker = dir.path().join("params-is-a-file");
        std::fs::write(&blocker, b"x").unwrap();

        check_output_dirs(
            &dir.path().join("work"),
            &dir.path().join("snark"),
            &dir.path().join("pkcache"),
            None,
        )
        .expect("a warm run never writes to params_dir, so it must not be probed");
    }

    #[test]
    fn output_dirs_check_creates_what_is_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        make_artifact_tree(dir.path());
        let snark = dir.path().join("snark/nested");

        check_output_dirs(
            &dir.path().join("work"),
            &snark,
            &dir.path().join("params/pk_cache"),
            Some(&dir.path().join("params")),
        )
        .expect("missing output dirs are created, not refused");
        assert!(snark.is_dir());
        assert!(
            dir.path().join("params/pk_cache").is_dir(),
            "default pk_cache must be created"
        );
    }
}
