//! Command-line surface + pure validators.
//!
//! This module is deliberately side-effect free — every fn here either
//! succeeds and produces a validated typed value or returns a
//! [`CliError::ArgInvalid`] / [`CliError::KeyFilePerms`]. No GraphQL, no
//! filesystem writes, no signing. That property lets the whole preflight
//! surface be unit-tested without a live network.
//!
//! Naming rationale (from Ekaterina's spec):
//! - `--from` / `--to` — the two sides of the bridge; the value's shape carries
//!   the type (the flag name doesn't say "address").
//! - `--from-keys` — keys *to what's in --from*; no owner_ prefix.
//! - `--amount` — token is fixed (USDC); putting the token in the flag name is
//!   a trap that breaks the moment a second token arrives.
//! - `--to-chain` — bridge has two sides, `--chain-id` would be ambiguous once
//!   the source side is selectable too.

use std::{path::PathBuf, str::FromStr};

use alloy_primitives::Address;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;

use crate::errors::{CliError, CliResult};

/// Amount is fixed-precision USDC. 6 decimals is the ERC-20 canonical
/// precision on every chain the bridge supports. Anything finer than
/// 1e-6 USDC is a spec violation, not something to round.
pub const USDC_DECIMALS: u32 = 6;

/// EVM chain-id whitelist for `--to-chain`. Source of truth for supported
/// destinations. Kept explicit (not a range) because the spec's rule is
/// "unknown chain → refuse, naming supported set" — a wildcard would let
/// a typo silently pass.
///
/// TODO(v2): consider pulling this from the `deposit-chain-ids` crate to
/// share one list with the deposit side. For v1 we duplicate to keep the
/// dep footprint tight and avoid a cross-workspace path.
pub const SUPPORTED_CHAINS: &[(u64, &str)] = &[
    (1, "Ethereum mainnet"),
    (11155111, "Sepolia (testnet)"),
    (8453, "Base"),
    (84532, "Base Sepolia"),
];

#[derive(Debug, Parser)]
#[command(
    name = "ackinacki-bridge",
    version,
    about = "Acki Nacki ↔ EVM bridge CLI. Currently ships the `withdraw` subcommand.",
    long_about = "Composes a single-custodian multisig sendTransaction that calls \
                  USDCBridge.initiateWithdrawal, waits for the WithdrawalInitiated event, \
                  resurrects the prover's mirror of `AckiNackiBridge` state from the on-chain \
                  contract at --bridge-address, waits for the covering L1/L2 anchor bundle to \
                  land (fed by a relayer running on some other host), produces the Circuit-4 \
                  SHPLONK proof, and submits withdrawByProof on the EVM side.\n\nThird-party \
                  end-user CLI: expects only an EVM RPC URL and the deployed AckiNackiBridge \
                  address — no local `prover_state.json`, no daemon on this machine."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,

    /// Machine-readable output on stdout (one line JSON). Human logs go to
    /// stderr regardless. Error output under --json is a single JSON
    /// object with {"error": {...}}.
    #[arg(long, global = true)]
    pub json: bool,

    /// Skip the terminal confirmation prompt. Intended for scripts.
    /// Combines with --non-interactive: --yes answers the question, and
    /// --non-interactive guarantees nothing will ever wait for an answer.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Refuse (exit 2) instead of prompting when a confirmation would be
    /// needed. Use in unattended contexts where waiting for input would
    /// hang.
    #[arg(long, global = true)]
    pub non_interactive: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Withdraw USDC from an AN multisig to an EVM recipient.
    Withdraw(WithdrawArgs),
}

#[derive(Debug, clap::Args)]
pub struct WithdrawArgs {
    /// Source multisig address in `dapp_id::account_id` form (both 64 hex,
    /// no `0x`, no workchain prefix). Must be an active, deployed
    /// single-custodian multisig.
    #[arg(long, value_name = "dapp_id::account_id")]
    pub from: String,

    /// Path to the multisig owner's keys.json. Must be a regular file,
    /// owned by the current uid, mode exactly `0400` (read-only for the
    /// owner — the CLI never writes this file). Otherwise the CLI refuses
    /// with `chmod 400 <path>`.
    #[arg(long, value_name = "PATH")]
    pub from_keys: PathBuf,

    /// EVM recipient. Accepts:
    /// - `0x…` (20-byte hex); mixed-case must pass EIP-55 checksum
    /// - CAIP-10 form `eip155:<chain>:<0x…>` (chain redundantly encoded; must
    ///   match --to-chain if that is also supplied)
    #[arg(long, value_name = "0x… | eip155:<chain>:<0x…>")]
    pub to: String,

    /// Numeric EIP-155 chain id of the destination EVM network. Required
    /// unless --to is CAIP-10 (in which case it's inferred and re-validated
    /// against the whitelist). No default — the cost of a wrong chain
    /// is irreversible.
    #[arg(long, value_name = "u64")]
    pub to_chain: Option<u64>,

    /// USDC amount as a decimal string. At most 6 fractional digits. Any
    /// finer precision is refused (not rounded).
    #[arg(long, value_name = "USDC")]
    pub amount: String,

    /// Preflight only: nothing is broadcast on either side and no
    /// idempotency state is recorded.
    ///
    /// Runs every check that needs no submit-only flag — the Acki Nacki
    /// account, multisig and key checks, and the EVM side (chain id,
    /// bridge deploy, verifier stack, pinned identity, treasury).
    ///
    /// Does NOT compose or sign the burn message, and does NOT check the
    /// prover artifacts (`--params-dir`, `--verifiers-dir`,
    /// `--aggregator-dir`): those need the submit-only flags a dry run
    /// does not require. Pass them anyway and `--verifiers-dir` will also
    /// be used to compare against the deployed verifier.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the "refuse duplicate in-flight withdrawal" refusal. v1
    /// blunt override — v2 will replace this with `--resume`.
    #[arg(long)]
    pub allow_retry: bool,

    /// Accept a `--verifiers-dir` whose
    /// `BridgeWithdrawalAggregatorVerifier.bin` differs from the one this
    /// build embeds. Correct only when you deployed your own bridge and
    /// regenerated the verifier.
    ///
    /// This is NOT the counterpart of `aggregate-proof --allow-bin-drift`.
    /// That one is a bootstrap escape hatch "for the very first bootstrap
    /// of a verifier whose .bin is not committed yet — never use once a
    /// verifier is deployed" (`aggregate_proof.rs:62-63`). Against a live
    /// self-deploy the regenerated bytecode must still equal your own
    /// deployed `.bin`, and suppressing that comparison hides a proof the
    /// chain will reject.
    ///
    /// On the pinned deploy a mismatch means a stale or corrupted file, and
    /// passing this flag will not make the proof verify on-chain.
    #[arg(long)]
    pub allow_verifier_drift: bool,

    // -- Environment / plumbing --
    /// GraphQL endpoint for the AN chain (event capture + account queries).
    #[arg(long, env = "BRIDGE_GQL_ENDPOINT", value_name = "URL")]
    pub gql_endpoint: String,

    /// On-chain USDCBridge account id (64 hex, no `0x`). No compiled
    /// default — supplied per network via `USDC_BRIDGE_ACCOUNT_ID` in
    /// the profile file (see `config/bridge_config.shellnet` for the
    /// shellnet palindromic `1a1a…1a1a`). The dapp_id is resolved live
    /// via GraphQL, so only the account id is needed here.
    #[arg(long, env = "USDC_BRIDGE_ACCOUNT_ID")]
    pub usdc_bridge_account: String,

    /// Anchor layer selection passed through to the enricher. Use `auto`
    /// (default) on L1 deploys; `2` with `--i-know-the-wait` on L2.
    #[arg(
        long,
        env = "BRIDGE_ANCHOR_LAYER",
        default_value = "auto",
        value_name = "auto|1|2"
    )]
    pub anchor_layer: String,

    /// Acknowledge the L≥2 wait budget (up to ~101 min chain-time for L2).
    /// Required by the enricher when `--anchor-layer` is explicit ≥2.
    /// Env: `BRIDGE_I_KNOW_THE_WAIT=true|false`.
    #[arg(long, env = "BRIDGE_I_KNOW_THE_WAIT")]
    pub i_know_the_wait: bool,

    // -- ETH side (submit) --
    /// EVM JSON-RPC endpoint.
    #[arg(long, env = "RPC_URL", value_name = "URL")]
    pub rpc_url: String,

    /// Deployed AckiNackiBridge address on the destination chain.
    #[arg(long, env = "BRIDGE_ADDRESS")]
    pub bridge_address: Address,

    /// Signer key for the EVM `withdrawByProof` tx. Distinct from
    /// `--from-keys` (which signs on AN). Typically the operator's ETH
    /// gas wallet; the recipient of the USDC is `--to`, not this signer.
    #[arg(long, env = "BURNER_PRIVATE_KEY", value_name = "0x…")]
    pub eth_private_key: Option<String>,

    // -- Prover subprocess plumbing (passed through to run_once) --
    #[arg(long, env = "BRIDGE_AGGREGATOR_DIR")]
    pub aggregator_dir: Option<PathBuf>,
    #[arg(long, env = "BRIDGE_VERIFIERS_DIR")]
    pub verifiers_dir: Option<PathBuf>,
    #[arg(long, env = "BRIDGE_PARAMS_DIR")]
    pub params_dir: Option<PathBuf>,
    #[arg(long, env = "BRIDGE_SNARK_DIR", default_value = "./shplonk-snark")]
    pub snark_dir: PathBuf,
    #[arg(long, env = "BRIDGE_PK_CACHE_DIR")]
    pub pk_cache_dir: Option<PathBuf>,
    #[arg(long)]
    pub prover_out_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 1800)]
    pub prover_timeout_s: u64,
    #[arg(long, env = "BRIDGE_WORK_DIR")]
    pub work_dir: Option<PathBuf>,

    // -- Idempotency --
    /// Directory holding per-withdrawal state files. Defaults to
    /// `$HOME/.bridge-withdraw-state/` — per-user, survives tree moves.
    /// Profile files typically override this to a per-deploy path.
    #[arg(long, env = "BRIDGE_WITHDRAW_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
}

// -- Validated forms of raw args, produced by [`WithdrawArgs::validate`] --

/// Parsed & validated `--from`. Both halves are strict 64-char lowercase
/// hex, no prefix, no workchain. For self-rooted multisigs `dapp_id ==
/// account_id` by convention (not enforced here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FromAddress {
    pub dapp_id_hex: String,
    pub account_id_hex: String,
}

impl FromAddress {
    /// Legacy `0:<account>` form used inside ABI-encoded payload `dest`
    /// fields (multisig sendTransaction).
    pub fn legacy(&self) -> String {
        format!("0:{}", self.account_id_hex)
    }

    /// The `dapp_id::account_id` form the tvm-cli v3 `--addr` flag expects.
    pub fn extended(&self) -> String {
        format!("{}::{}", self.dapp_id_hex, self.account_id_hex)
    }
}

/// Parsed & validated `--to` + `--to-chain`. Both post-validation, so
/// downstream code can trust the chain id is on the whitelist and the
/// address is a real 20-byte EIP-55-valid EVM address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToAddress {
    pub address: Address,
    pub chain_id: u64,
}

/// Fixed-precision USDC amount in "micro" units (10^-6 USDC), matching
/// the on-chain ERC-20 representation and the `cc` map value passed to
/// multisig `sendTransaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsdcAmount(pub u128);

impl UsdcAmount {
    /// Human-readable decimal string with exactly 6 fractional digits.
    pub fn display(&self) -> String {
        let whole = self.0 / 1_000_000;
        let frac = self.0 % 1_000_000;
        format!("{whole}.{frac:06}")
    }
}

// -- Validators --

/// Parse `dapp_id::account_id`. Both halves must be exactly 64 lowercase
/// hex characters; the separator is exactly `::`; no leading `0:` and no
/// workchain prefix.
pub fn parse_from(raw: &str) -> CliResult<FromAddress> {
    let (dapp, acc) = raw.split_once("::").ok_or_else(|| CliError::ArgInvalid {
        flag: "from",
        expected: "dapp_id::account_id (both 64 hex, no 0x, no workchain)".into(),
        got: redact(raw),
    })?;
    if !is_64_hex(dapp) || !is_64_hex(acc) {
        return Err(CliError::ArgInvalid {
            flag: "from",
            expected: "each half exactly 64 lowercase hex chars".into(),
            got: crate::errors::Redacted::rendered(format!("{}::{}", redact(dapp), redact(acc))),
        });
    }
    Ok(FromAddress {
        dapp_id_hex: dapp.to_ascii_lowercase(),
        account_id_hex: acc.to_ascii_lowercase(),
    })
}

/// Parse `--to` (0x… or CAIP-10) and reconcile with `--to-chain`. The
/// chain-id whitelist is consulted here — unknown chain is a refusal per
/// spec, not a warning.
pub fn parse_to(raw: &str, to_chain: Option<u64>) -> CliResult<ToAddress> {
    let (addr_str, chain_id) = if let Some(caip) = raw.strip_prefix("eip155:") {
        // `<chain>:<0x…>`
        let (chain_str, addr) = caip.split_once(':').ok_or_else(|| CliError::ArgInvalid {
            flag: "to",
            expected: "eip155:<chain-id>:0x<20-byte-hex>".into(),
            got: redact(raw),
        })?;
        let chain = chain_str.parse::<u64>().map_err(|_| CliError::ArgInvalid {
            flag: "to",
            expected: "CAIP-10 chain segment must be numeric EIP-155 id".into(),
            got: redact(chain_str),
        })?;
        if let Some(explicit) = to_chain {
            if explicit != chain {
                return Err(CliError::ArgInvalid {
                    flag: "to-chain",
                    expected: format!("must match --to CAIP chain segment ({chain})"),
                    got: crate::errors::Redacted::rendered(explicit),
                });
            }
        }
        (addr, chain)
    } else {
        let chain = to_chain.ok_or_else(|| CliError::ArgInvalid {
            flag: "to-chain",
            expected: "required when --to is plain 0x… (no default — irreversible on wrong chain)"
                .into(),
            got: crate::errors::Redacted::rendered("<absent>"),
        })?;
        (raw, chain)
    };

    if !SUPPORTED_CHAINS.iter().any(|(id, _)| *id == chain_id) {
        return Err(CliError::ArgInvalid {
            flag: "to-chain",
            expected: format!("one of {}", format_supported_chains()),
            got: crate::errors::Redacted::rendered(chain_id),
        });
    }

    // EIP-55: alloy's Address::from_str is case-insensitive and does NOT
    // enforce the checksum. We explicitly opt into parse_checksummed when
    // the hex body has any uppercase letter, so a corrupted-checksum
    // mixed-case address is refused instead of silently accepted.
    let hex_body = addr_str.strip_prefix("0x").unwrap_or(addr_str);
    let is_mixed_case = hex_body.chars().any(|c| c.is_ascii_uppercase())
        && hex_body.chars().any(|c| c.is_ascii_lowercase());
    let address = if is_mixed_case {
        Address::parse_checksummed(addr_str, None).map_err(|e| CliError::ArgInvalid {
            flag: "to",
            expected: "mixed-case address must be valid EIP-55 checksum".into(),
            got: crate::errors::Redacted::rendered(format!("{} ({e})", redact(addr_str))),
        })?
    } else {
        Address::from_str(addr_str).map_err(|e| CliError::ArgInvalid {
            flag: "to",
            expected: "0x-prefixed 20-byte hex address".into(),
            got: crate::errors::Redacted::rendered(format!("{} ({e})", redact(addr_str))),
        })?
    };

    // Refuse the zero address unconditionally. `withdrawByProof` on the
    // deployed `AckiNackiBridge` transfers USDC to `pub.recipient`; a
    // successful submit against `0x0` would burn the treasury draw to
    // an unrecoverable address. The on-chain contract does not guard
    // this (the ERC-20 transfer may or may not, depending on the token
    // implementation) — belt-and-suspenders refusal at the CLI boundary
    // is the safer default.
    if address == Address::ZERO {
        return Err(CliError::ArgInvalid {
            flag: "to",
            expected: "non-zero EVM address (refuse burning to 0x0)".into(),
            got: crate::errors::Redacted::rendered(format!("{address:?}")),
        });
    }

    Ok(ToAddress {
        address,
        chain_id,
    })
}

/// Parse `--amount` as decimal USDC, reject > 6 fractional digits (no
/// rounding), reject non-positive.
pub fn parse_amount(raw: &str) -> CliResult<UsdcAmount> {
    let d = Decimal::from_str(raw).map_err(|_| CliError::ArgInvalid {
        flag: "amount",
        expected: "decimal number, e.g. 1.000000".into(),
        got: redact(raw),
    })?;
    if d.scale() > USDC_DECIMALS {
        return Err(CliError::ArgInvalid {
            flag: "amount",
            expected: format!("at most {USDC_DECIMALS} fractional digits (USDC precision)"),
            got: redact(raw),
        });
    }
    if d.is_sign_negative() || d.is_zero() {
        return Err(CliError::ArgInvalid {
            flag: "amount",
            expected: "positive USDC amount".into(),
            got: redact(raw),
        });
    }
    // Scale up to micro-USDC. `d * 10^6` cannot lose precision because we
    // just verified scale ≤ 6.
    let scaled = d
        .checked_mul(Decimal::new(1_000_000, 0))
        .ok_or_else(|| CliError::ArgInvalid {
            flag: "amount",
            expected: "value fits in u128 micro-USDC".into(),
            got: redact(raw),
        })?;
    let micros: u128 = scaled
        .trunc()
        .try_into()
        .map_err(|_| CliError::ArgInvalid {
            flag: "amount",
            expected: "value fits in u128 micro-USDC".into(),
            got: redact(raw),
        })?;
    // Cap at u64::MAX micro-USDC. The multisig ECC[3] balance and the
    // AN-side `initiateWithdrawal(amount)` argument are u64 on the wire;
    // anything above 2^64 - 1 micro-USDC (~1.8e13 USDC) cannot be
    // burned even with an over-funded multisig, and letting it through
    // would trip a downstream cast in `burn::compose` at broadcast time
    // rather than a clean preflight refusal.
    if micros > u64::MAX as u128 {
        return Err(CliError::ArgInvalid {
            flag: "amount",
            expected: format!(
                "must fit in u64 micro-USDC (max {} USDC)",
                u64::MAX / 1_000_000
            ),
            got: redact(raw),
        });
    }
    Ok(UsdcAmount(micros))
}

/// The one mode `--from-keys` may have.
///
/// Read-only for the owner. The CLI reads this file and nothing else, so a
/// write bit buys nothing and an execute bit is meaningless; pinning the
/// exact value makes the requirement checkable from a script instead of
/// approximately describable in prose.
pub const KEY_FILE_MODE: u32 = 0o400;

/// Enforce that `--from-keys` is a regular file owned by the current uid
/// and mode is exactly [`KEY_FILE_MODE`]. Each failure names its own remedy —
/// telling someone to `chmod 400` a path that does not exist wastes a
/// round trip and hides the real problem.
pub fn check_key_file_perms(path: &std::path::Path) -> CliResult<()> {
    use std::os::unix::fs::MetadataExt;

    let refuse = |problem: String| CliError::KeyFilePerms {
        path: path.display().to_string(),
        problem,
    };

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refuse("does not exist".into()));
        },
        Err(e) => {
            return Err(refuse(format!("cannot stat: {e}")));
        },
    };
    if !meta.is_file() {
        return Err(refuse("not a regular file".into()));
    }
    let uid_now = unsafe { libc_getuid() };
    if meta.uid() != uid_now {
        return Err(refuse(format!(
            "owned by uid {} but this process runs as uid {uid_now}",
            meta.uid()
        )));
    }
    // Exactly 0400. The CLI only ever reads this file — it never writes,
    // rotates or appends — so read-only-to-owner is the tightest mode that
    // still works, and an exact match is a contract an operator and a
    // script can both check. "Owner-only" as a range would also admit 0600
    // and 0700, which grant a write and an execute bit nothing needs.
    //
    // This refuses 0600, which is what today's README tells people to set,
    // so the message has to be immediately actionable and the changelog
    // entry is a breaking change.
    let mode = meta.mode() & 0o777;
    if mode != KEY_FILE_MODE {
        return Err(refuse(format!(
            "mode is {mode:04o}, must be exactly {KEY_FILE_MODE:04o} (read-only for the owner; \
             the CLI never writes this file); run: chmod 400 {}",
            path.display()
        )));
    }
    Ok(())
}

// libc::getuid is a syscall we don't need a full libc dep for. Bind it
// directly to avoid pulling `libc` into the graph for one number.
#[allow(non_snake_case)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

// -- Helpers --

fn is_64_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Truncate an untrusted input string for safe echo back in errors. Never
/// used for anything that could be a key or secret — but as a belt-and-
/// suspenders default we clip long inputs to 24 characters.
///
/// Every argument here comes straight from argv, so this function is on
/// the path of malformed input by construction and must not be the thing
/// that fails on it. `&s[..24]` panicked whenever byte 24 landed inside a
/// multibyte character: `--to` with an emoji at the wrong offset turned a
/// clean "invalid address" refusal into exit 101 and a message no
/// consumer could parse, which is exactly what the `--json` envelope
/// exists to prevent. Counting characters also makes N mean what the
/// sentence above says it means.
pub(crate) fn redact(s: &str) -> crate::errors::Redacted {
    const N: usize = 24;
    let mut head = String::new();
    let mut rest = s.chars();
    for c in rest.by_ref().take(N) {
        // Control characters survive argv and land in a multi-line
        // refusal: a bare newline forges a line the CLI never wrote, and
        // an ANSI escape repaints the terminal the refusal is read on.
        if c.is_control() {
            head.extend(c.escape_debug());
        } else {
            head.push(c);
        }
    }
    if rest.next().is_some() {
        head.push('…');
    }
    crate::errors::Redacted::rendered(head)
}

fn format_supported_chains() -> String {
    SUPPORTED_CHAINS
        .iter()
        .map(|(id, name)| format!("{id} ({name})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The plumbing a real (non-`--dry-run`) withdrawal needs but a preflight
/// does not. Collected in one place so a real run refuses ONCE, naming
/// every missing value, instead of dying on the first `.unwrap()` three
/// stages in.
#[derive(Debug, Clone)]
pub struct SubmitPlumbing {
    pub eth_private_key: String,
    pub aggregator_dir: PathBuf,
    pub verifiers_dir: PathBuf,
    pub params_dir: PathBuf,
    pub work_dir: PathBuf,
}

impl WithdrawArgs {
    /// Resolve the submit-only plumbing, or refuse listing everything that
    /// is missing. `--dry-run` never calls this — that is the whole point:
    /// a preflight must not require an EVM signing key it will never use.
    pub fn require_submit_plumbing(&self) -> CliResult<SubmitPlumbing> {
        let mut missing: Vec<&str> = Vec::new();
        if self.eth_private_key.is_none() {
            missing.push("--eth-private-key (BURNER_PRIVATE_KEY)");
        }
        if self.aggregator_dir.is_none() {
            missing.push("--aggregator-dir (BRIDGE_AGGREGATOR_DIR)");
        }
        if self.verifiers_dir.is_none() {
            missing.push("--verifiers-dir (BRIDGE_VERIFIERS_DIR)");
        }
        if self.params_dir.is_none() {
            missing.push("--params-dir (BRIDGE_PARAMS_DIR)");
        }
        if self.work_dir.is_none() {
            missing.push("--work-dir (BRIDGE_WORK_DIR)");
        }
        if !missing.is_empty() {
            return Err(CliError::Preflight {
                reason: format!(
                    "a real withdrawal needs {}. Set them in the profile file pointed to by \
                     $BRIDGE_CONFIG, or pass them explicitly. (--dry-run does not need any of \
                     them.)",
                    missing.join(", "),
                ),
                source: None,
            });
        }
        Ok(SubmitPlumbing {
            eth_private_key: self.eth_private_key.clone().expect("checked above"),
            aggregator_dir: self.aggregator_dir.clone().expect("checked above"),
            verifiers_dir: self.verifiers_dir.clone().expect("checked above"),
            params_dir: self.params_dir.clone().expect("checked above"),
            work_dir: self.work_dir.clone().expect("checked above"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parses_lowercase_hex() {
        let a = "abababababababababababababababababababababababababababababababab";
        let b = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let parsed = parse_from(&format!("{a}::{b}")).unwrap();
        assert_eq!(parsed.dapp_id_hex, a);
        assert_eq!(parsed.account_id_hex, b);
        assert_eq!(parsed.legacy(), format!("0:{b}"));
        assert_eq!(parsed.extended(), format!("{a}::{b}"));
    }

    #[test]
    fn from_rejects_0x_prefix() {
        let raw = format!("0x{}::{}", "a".repeat(64), "b".repeat(64));
        assert!(matches!(
            parse_from(&raw),
            Err(CliError::ArgInvalid {
                flag: "from",
                ..
            })
        ));
    }

    #[test]
    fn from_rejects_workchain_prefix() {
        let raw = format!("0:{}", "a".repeat(64));
        assert!(matches!(
            parse_from(&raw),
            Err(CliError::ArgInvalid {
                flag: "from",
                ..
            })
        ));
    }

    #[test]
    fn amount_rejects_seven_decimals() {
        assert!(matches!(
            parse_amount("1.0000001"),
            Err(CliError::ArgInvalid {
                flag: "amount",
                ..
            })
        ));
    }

    #[test]
    fn amount_accepts_six_decimals_exact() {
        let m = parse_amount("1.234567").unwrap();
        assert_eq!(m.0, 1_234_567);
        assert_eq!(m.display(), "1.234567");
    }

    #[test]
    fn amount_rejects_zero_and_negative() {
        assert!(parse_amount("0").is_err());
        assert!(parse_amount("0.0").is_err());
        assert!(parse_amount("-1.0").is_err());
    }

    #[test]
    fn amount_accepts_integer() {
        let m = parse_amount("5").unwrap();
        assert_eq!(m.0, 5_000_000);
    }

    #[test]
    fn to_requires_chain_when_bare_hex() {
        let raw = "0x742d35Cc6634C0532925a3b844Bc454e4438f44e";
        assert!(matches!(
            parse_to(raw, None),
            Err(CliError::ArgInvalid {
                flag: "to-chain",
                ..
            })
        ));
    }

    #[test]
    fn to_accepts_caip10_and_infers_chain() {
        let raw = "eip155:11155111:0x742d35Cc6634C0532925a3b844Bc454e4438f44e";
        let t = parse_to(raw, None).unwrap();
        assert_eq!(t.chain_id, 11155111);
    }

    #[test]
    fn to_rejects_caip10_chain_mismatch() {
        let raw = "eip155:1:0x742d35Cc6634C0532925a3b844Bc454e4438f44e";
        assert!(matches!(
            parse_to(raw, Some(11155111)),
            Err(CliError::ArgInvalid {
                flag: "to-chain",
                ..
            })
        ));
    }

    #[test]
    fn to_rejects_unknown_chain() {
        let raw = "0x742d35Cc6634C0532925a3b844Bc454e4438f44e";
        assert!(matches!(
            parse_to(raw, Some(999)),
            Err(CliError::ArgInvalid {
                flag: "to-chain",
                ..
            })
        ));
    }

    #[test]
    fn a_multibyte_argument_is_refused_not_panicked_on() {
        // `&s[..24]` panicked whenever byte 24 fell inside a character.
        // Every one of these is 24 ASCII bytes followed by one that is
        // not, so the old slice split it. The result was exit 101 and a
        // message no `--json` consumer could parse — from an argument
        // whose only sin was being wrong.
        let head = "0x742d35Cc6634C0532";
        assert_eq!(head.len(), 19);
        for tail in ["привет", "日本語", "🙂🙂", "e\u{301}\u{301}"] {
            let raw = format!("{head}{tail}");
            let res = parse_to(&raw, Some(11155111));
            assert!(
                matches!(
                    res,
                    Err(CliError::ArgInvalid {
                        flag: "to",
                        ..
                    })
                ),
                "{raw:?} must be refused, got {res:?}",
            );
        }
    }

    #[test]
    fn redact_clips_characters_and_neutralises_control_bytes() {
        // The clip is 24 CHARACTERS, and it always leaves a valid string.
        let long = "\u{444}".repeat(40);
        let out = redact(&long);
        assert_eq!(
            out.as_str().chars().count(),
            25,
            "24 characters plus the ellipsis"
        );
        assert!(out.as_str().ends_with('…'));
        // Exactly 24 characters is not truncated, and carries no ellipsis.
        let exact = "\u{444}".repeat(24);
        assert_eq!(redact(&exact).as_str(), exact);
        // A newline in argv would otherwise forge a line inside a
        // multi-line refusal, and an ANSI escape would repaint the
        // terminal the refusal is being read on.
        assert_eq!(redact("a\nb").as_str(), "a\\nb");
        assert_eq!(redact("a\u{1b}[2Jb").as_str(), "a\\u{1b}[2Jb");
    }

    #[test]
    fn to_rejects_zero_address() {
        // Sergey review R7 (2026-09-01): refuse burns to 0x0 at the CLI
        // boundary. The on-chain contract's ERC-20 transfer to 0x0 could
        // succeed depending on the token implementation, permanently
        // consuming a treasury draw for an unrecoverable recipient.
        let raw = "0x0000000000000000000000000000000000000000";
        let res = parse_to(raw, Some(11155111));
        assert!(
            matches!(
                res,
                Err(CliError::ArgInvalid {
                    flag: "to",
                    ..
                })
            ),
            "0x0 recipient must be refused, got {res:?}",
        );
    }

    #[test]
    fn amount_rejects_above_u64_max_micro() {
        // Sergey review R8 (2026-09-01): the multisig ECC[3] balance and
        // AN-side initiateWithdrawal(amount) argument are u64 on the
        // wire. Refuse anything > u64::MAX micro-USDC (~1.8e13 USDC)
        // at preflight rather than tripping a downstream cast.
        // `--amount` is USDC (not micros): u64::MAX micros ==
        // 18446744073709.551615 USDC, so ".551616" is one micro over.
        let over = "18446744073709.551616";
        let res = parse_amount(over);
        assert!(
            matches!(
                res,
                Err(CliError::ArgInvalid {
                    flag: "amount",
                    ..
                })
            ),
            "one micro above u64::MAX must be refused, got {res:?}",
        );
    }

    #[test]
    fn amount_accepts_u64_max_micro_exact() {
        // Boundary: exactly u64::MAX micro-USDC must be accepted.
        // 18446744073709.551615 USDC == u64::MAX micro-USDC.
        let exact = "18446744073709.551615";
        let m = parse_amount(exact).expect("u64::MAX micro-USDC must be accepted");
        assert_eq!(m.0, u64::MAX as u128);
    }

    #[test]
    fn to_rejects_bad_eip55_mixed_case() {
        // Deliberately corrupted checksum (swap two case bits).
        let raw = "0x742D35Cc6634c0532925a3b844Bc454e4438f44e";
        let res = parse_to(raw, Some(11155111));
        assert!(matches!(
            res,
            Err(CliError::ArgInvalid {
                flag: "to",
                ..
            })
        ));
    }

    #[test]
    fn to_accepts_all_lowercase() {
        let raw = "0x742d35cc6634c0532925a3b844bc454e4438f44e";
        assert!(parse_to(raw, Some(11155111)).is_ok());
    }

    fn submit_plumbing_fixture_all_absent() -> WithdrawArgs {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "ackinacki-bridge",
            "withdraw",
            "--from",
            &format!("{}::{}", "ab".repeat(32), "cd".repeat(32)),
            "--from-keys",
            "/dev/null",
            "--to",
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "--to-chain",
            "11155111",
            "--amount",
            "1.000000",
            "--gql-endpoint",
            "https://example.invalid/graphql",
            "--usdc-bridge-account",
            &"1a".repeat(32),
            "--rpc-url",
            "https://example.invalid/rpc",
            "--bridge-address",
            "0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7",
        ])
        .expect("parse");
        let Command::Withdraw(args) = cli.cmd;
        args
    }

    #[test]
    fn dry_run_does_not_require_submit_plumbing() {
        use clap::Parser;
        // --rpc-url and --bridge-address ARE still required (see the note on
        // this task): a dry-run checks the destination chain id, which needs
        // the endpoint but no key. What must not be required is the signing
        // key and the four prover directories.
        let cli = Cli::try_parse_from([
            "ackinacki-bridge",
            "withdraw",
            "--from",
            &format!("{}::{}", "ab".repeat(32), "cd".repeat(32)),
            "--from-keys",
            "/dev/null",
            "--to",
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "--to-chain",
            "11155111",
            "--amount",
            "1.000000",
            "--dry-run",
            "--yes",
            "--gql-endpoint",
            "https://example.invalid/graphql",
            "--usdc-bridge-account",
            &"1a".repeat(32),
            "--rpc-url",
            "https://example.invalid/rpc",
            "--bridge-address",
            "0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7",
        ])
        .expect("--dry-run must parse without --eth-private-key or the prover dirs");
        let Command::Withdraw(args) = cli.cmd;
        assert!(args.dry_run);
        assert!(args.eth_private_key.is_none());
        assert!(args.params_dir.is_none());
        assert!(args.work_dir.is_none());
    }

    #[test]
    fn real_run_names_every_missing_submit_flag_at_once() {
        let args = submit_plumbing_fixture_all_absent();
        let err = args
            .require_submit_plumbing()
            .expect_err("a real run without plumbing must refuse");
        let msg = format!("{err}");
        for flag in [
            "--eth-private-key",
            "--aggregator-dir",
            "--verifiers-dir",
            "--params-dir",
            "--work-dir",
        ] {
            assert!(msg.contains(flag), "refusal must name {flag}, got: {msg}");
        }
        assert!(
            msg.contains("BRIDGE_CONFIG"),
            "refusal must point at the profile file, got: {msg}"
        );
    }

    #[test]
    fn yes_and_non_interactive_can_be_combined() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "ackinacki-bridge",
            "--yes",
            "--non-interactive",
            "withdraw",
            "--from",
            &format!("{}::{}", "ab".repeat(32), "cd".repeat(32)),
            "--from-keys",
            "/dev/null",
            "--to",
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "--to-chain",
            "11155111",
            "--amount",
            "1.000000",
            "--gql-endpoint",
            "https://example.invalid/graphql",
            "--usdc-bridge-account",
            &"1a".repeat(32),
            "--rpc-url",
            "https://example.invalid/rpc",
            "--bridge-address",
            "0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7",
        ])
        .expect("a CI wrapper must be able to set both belt and braces");
        assert!(cli.yes);
        assert!(cli.non_interactive);
    }

    #[test]
    fn key_file_refusal_distinguishes_missing_from_loose() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();

        let missing = dir.path().join("nope.json");
        let err = check_key_file_perms(&missing).expect_err("missing file must refuse");
        let msg = format!("{err}");
        assert!(msg.contains("does not exist"), "got: {msg}");
        assert!(
            !msg.contains("chmod 400"),
            "chmod is useless here, got: {msg}"
        );

        let a_dir = dir.path();
        let err = check_key_file_perms(a_dir).expect_err("a directory must refuse");
        let msg = format!("{err}");
        assert!(msg.contains("not a regular file"), "got: {msg}");

        let loose = dir.path().join("loose.json");
        std::fs::write(&loose, "{}").unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = check_key_file_perms(&loose).expect_err("0644 must refuse");
        let msg = format!("{err}");
        assert!(msg.contains("chmod 400"), "got: {msg}");
        assert!(
            msg.contains("0644") || msg.contains("644"),
            "must name the mode, got: {msg}"
        );
    }

    #[test]
    fn key_file_mode_is_exactly_0400() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();

        let accepted = dir.path().join("ok.json");
        std::fs::write(&accepted, "{}").unwrap();
        std::fs::set_permissions(&accepted, std::fs::Permissions::from_mode(0o400)).unwrap();
        check_key_file_perms(&accepted).expect("0400 is the required mode");

        // Every other owner-only mode is refused too. 0600 is the important
        // one: it is what today's README tells operators to set, so this is
        // the assertion that pins the breaking change.
        for mode in [0o600u32, 0o700, 0o500, 0o440, 0o000] {
            let f = dir.path().join(format!("m{mode:o}.json"));
            std::fs::write(&f, "{}").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = check_key_file_perms(&f).expect_err(&format!(
                "mode {mode:04o} must be refused, but was accepted"
            ));
            let msg = format!("{err}");
            assert!(
                msg.contains("chmod 400"),
                "mode {mode:04o} must be refused with the exact remedy, got: {msg}",
            );
        }
    }
}
