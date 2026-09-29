//! `deposit`'s command line and its validated form.

use std::{path::PathBuf, str::FromStr, time::Duration};

use alloy_primitives::{Address, B256, U256};
use serde_json::json;

use crate::{
    args::{parse_amount, redact, UsdcAmount},
    errors::{CliError, CliResult},
};

/// EVM networks a deposit can be made from. Each one must be a chain the
/// deposit circuit accepts; mainnet is not one of them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Sepolia,
}

impl Network {
    pub const ALL: &'static [Network] = &[Network::Sepolia];

    pub fn chain_id(self) -> u64 {
        match self {
            Network::Sepolia => deposit_chain_ids::CHAIN_ID_SEPOLIA,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Network::Sepolia => "Sepolia",
        }
    }

    /// The CAIP-2 id WalletConnect namespaces use.
    pub fn caip2(self) -> String {
        format!("eip155:{}", self.chain_id())
    }

    /// The `wallet_addEthereumChain` payload (EIP-3085).
    pub fn eip3085(self) -> serde_json::Value {
        match self {
            Network::Sepolia => json!({
                "chainId": format!("0x{:x}", self.chain_id()),
                "chainName": "Sepolia",
                "nativeCurrency": { "name": "Sepolia Ether", "symbol": "ETH", "decimals": 18 },
                "rpcUrls": ["https://ethereum-sepolia-rpc.publicnode.com"],
                "blockExplorerUrls": ["https://sepolia.etherscan.io"],
            }),
        }
    }

    pub fn from_chain_id(id: u64) -> Option<Network> {
        Network::ALL.iter().copied().find(|n| n.chain_id() == id)
    }
}

/// The Acki Nacki recipient, `dapp_id::account_id`. The dapp is the
/// user's claim about where the account lives; preflight checks it
/// against the account when that account is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnTarget {
    pub dapp_id: [u8; 32],
    pub account_id: [u8; 32],
}

impl AnTarget {
    pub fn parse(raw: &str) -> CliResult<AnTarget> {
        let bad = |expected: &str| CliError::ArgInvalid {
            flag: "to",
            expected: expected.into(),
            got: redact(raw),
        };
        let (dapp, acc) = raw
            .split_once("::")
            .ok_or_else(|| bad("dapp_id::account_id (both 64 hex, no 0x, no workchain)"))?;
        let decode = |half: &str| -> Option<[u8; 32]> {
            if half.len() != 64 || !half.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let mut out = [0u8; 32];
            hex::decode_to_slice(half, &mut out).ok()?;
            Some(out)
        };
        let (Some(dapp_id), Some(account_id)) = (decode(dapp), decode(acc)) else {
            return Err(bad("each half exactly 64 hex characters"));
        };
        if account_id == [0u8; 32] {
            return Err(bad(
                "a non-zero account id: the bridge refuses a zero recipient"
            ));
        }
        Ok(AnTarget {
            dapp_id,
            account_id,
        })
    }

    pub fn dapp_hex(&self) -> String {
        hex::encode(self.dapp_id)
    }

    pub fn account_hex(&self) -> String {
        hex::encode(self.account_id)
    }

    pub fn extended(&self) -> String {
        format!("{}::{}", self.dapp_hex(), self.account_hex())
    }

    pub fn account_b256(&self) -> B256 {
        B256::from(self.account_id)
    }
}

/// How the transactions reach the wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum QrMode {
    Walletconnect,
    Eip681,
    Both,
}

/// What `--resume` names: an operation, or — once the deposit is known —
/// the bridge's deposit id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpRef {
    Op(String),
    DepositId(U256),
}

impl OpRef {
    pub fn parse(raw: &str) -> CliResult<OpRef> {
        if raw.len() == 26 && ulid::Ulid::from_string(raw).is_ok() {
            return Ok(OpRef::Op(raw.to_ascii_uppercase()));
        }
        U256::from_str(raw)
            .map(OpRef::DepositId)
            .map_err(|_| CliError::ArgInvalid {
                flag: "resume",
                expected: "an operation id (26 characters) or a deposit id".into(),
                got: redact(raw),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    Fresh,
    DryRun,
    Resume {
        target: OpRef,
        tx_hash: Option<B256>,
    },
    Abandon(String),
}

/// `--json`, `--yes`, `--non-interactive`, as `main` parsed them.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobalFlags {
    pub json: bool,
    pub yes: bool,
    pub non_interactive: bool,
}

/// `deposit`'s flags as clap parsed them, raw.
#[derive(Debug, clap::Args)]
pub struct DepositArgs {
    /// EVM network the deposit is made on.
    #[arg(long, value_enum)]
    pub network: Option<Network>,
    /// USDC amount, at most 6 fractional digits (never rounded).
    #[arg(long, value_name = "USDC")]
    pub amount: Option<String>,
    /// Acki Nacki recipient, `dapp_id::account_id`.
    #[arg(long, value_name = "dapp_id::account_id")]
    pub to: Option<String>,

    #[arg(long, env = "RPC_URL", value_name = "URL")]
    pub rpc_url: Option<String>,
    #[arg(long, env = "BRIDGE_ADDRESS")]
    pub bridge_address: Option<Address>,
    #[arg(long, env = "BRIDGE_GQL_ENDPOINT", value_name = "URL")]
    pub gql_endpoint: Option<String>,
    #[arg(long, env = "USDC_BRIDGE_ACCOUNT_ID")]
    pub usdc_bridge_account: Option<String>,
    /// Directory holding the deposit prover, `configs/circuit_params.json`
    /// and `data/`. The prover runs with this as its working directory.
    #[arg(long, env = "BRIDGE_DEPOSIT_PROVER_DIR")]
    pub deposit_prover_dir: Option<PathBuf>,
    #[arg(long, env = "BRIDGE_DEPOSIT_CONFIRMATIONS", default_value_t = 12)]
    pub confirmations: u64,
    /// Deposit operations, their locks and their claims. Keep it the same
    /// for one sender: operations in another directory are invisible here.
    #[arg(long, env = "BRIDGE_DEPOSIT_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    #[arg(long, env = "BRIDGE_WORK_DIR")]
    pub work_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 1800)]
    pub prover_timeout_s: u64,
    /// 0 waits forever.
    #[arg(long, default_value_t = 0)]
    pub anchor_timeout_s: u64,
    #[arg(long, default_value_t = 120)]
    pub relayer_grace_s: u64,
    #[arg(long, default_value_t = 600)]
    pub recovery_window_s: u64,
    #[arg(long, default_value_t = 300)]
    pub credit_timeout_s: u64,
    #[arg(long, default_value_t = 300)]
    pub pair_timeout_s: u64,

    #[arg(long, value_enum, default_value_t = QrMode::Walletconnect)]
    pub qr_mode: QrMode,
    #[arg(long, value_name = "PATH")]
    pub qr_out: Option<PathBuf>,
    #[arg(long)]
    pub uri_only: bool,
    #[arg(long)]
    pub qr_invert: bool,
    #[arg(long, env = "BRIDGE_WC_PROJECT_ID")]
    pub wc_project_id: Option<String>,
    #[arg(long, default_value = "wss://relay.walletconnect.org")]
    pub wc_relay_url: String,
    /// The sending account. Checked against the wallet session in
    /// `walletconnect` mode; required in `eip681` and `both`.
    #[arg(long)]
    pub from_address: Option<Address>,

    /// Run preflight, print both transactions and the QR payload, send nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Continue an operation after an interruption or exit 30-34.
    #[arg(long, value_name = "OP_ID|DEPOSIT_ID", conflicts_with_all = ["abandon", "dry_run"])]
    pub resume: Option<String>,
    /// With --resume: bind this transaction when the search is ambiguous.
    #[arg(long, requires = "resume")]
    pub tx_hash: Option<B256>,
    /// Release an operation whose outcome is unknown, after checking in the
    /// wallet that its transaction does not exist.
    #[arg(long, value_name = "OP_ID", conflicts_with = "dry_run")]
    pub abandon: Option<String>,
}

/// The Acki Nacki network an endpoint points at, as an operation records
/// it: scheme, host and port (the scheme's default when none is given),
/// lowercased. The port counts: two local nodes on one host are two
/// networks, and `tvm_client` keeps an `http` endpoint's port for sending
/// too. A mirror of the same network elsewhere counts as another network
/// — resume refuses rather than guesses.
pub fn an_network_id(endpoint: &str) -> CliResult<String> {
    url::Url::parse(endpoint)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| {
            Some(format!(
                "{}://{}:{}",
                u.scheme(),
                u.host_str()?.to_ascii_lowercase(),
                u.port_or_known_default()?
            ))
        })
        .ok_or_else(|| CliError::ArgInvalid {
            flag: "gql-endpoint",
            expected: "an http(s) URL".into(),
            got: redact(endpoint),
        })
}

/// Compiled-in WalletConnect Cloud project id, set by the release build.
pub const DEFAULT_WC_PROJECT_ID: Option<&str> = option_env!("ACKINACKI_BRIDGE_WC_PROJECT_ID");

/// Everything a run needs, validated once.
#[derive(Debug, Clone)]
pub struct DepositParams {
    pub mode: RunMode,
    pub globals: GlobalFlags,
    pub network: Option<Network>,
    pub amount: Option<UsdcAmount>,
    pub to: Option<AnTarget>,
    pub rpc_url: Option<String>,
    pub bridge: Option<Address>,
    pub gql_endpoint: Option<String>,
    pub usdc_bridge_account: Option<[u8; 32]>,
    pub prover_dir: Option<PathBuf>,
    pub confirmations: u64,
    pub state_dir: PathBuf,
    pub work_dir: PathBuf,
    pub prover_timeout: Duration,
    pub anchor_timeout: Option<Duration>,
    pub relayer_grace: Duration,
    pub recovery_window: Duration,
    pub credit_timeout: Duration,
    pub pair_timeout: Duration,
    pub qr_mode: QrMode,
    pub qr_out: Option<PathBuf>,
    pub uri_only: bool,
    pub qr_invert: bool,
    pub wc_project_id: Option<String>,
    pub wc_relay_url: String,
    pub from_address: Option<Address>,
}

impl DepositArgs {
    pub fn validate(&self, g: &GlobalFlags) -> CliResult<DepositParams> {
        let mode = match (&self.abandon, &self.resume) {
            (Some(op), _) => RunMode::Abandon(match OpRef::parse(op)? {
                OpRef::Op(id) => id,
                OpRef::DepositId(_) => {
                    return Err(CliError::ArgInvalid {
                        flag: "abandon",
                        expected: "an operation id".into(),
                        got: redact(op),
                    })
                },
            }),
            (None, Some(r)) => RunMode::Resume {
                target: OpRef::parse(r)?,
                tx_hash: self.tx_hash,
            },
            (None, None) if self.dry_run => RunMode::DryRun,
            (None, None) => RunMode::Fresh,
        };
        let starts_new = matches!(mode, RunMode::Fresh | RunMode::DryRun);
        let touches_chains = !matches!(mode, RunMode::Abandon(_));
        // Shapes first: a malformed value is the user's first problem, not
        // the profile keys that happen to be missing as well.
        let amount = self.amount.as_deref().map(parse_amount).transpose()?;
        let to = self.to.as_deref().map(AnTarget::parse).transpose()?;

        let mut missing = Vec::new();
        if starts_new {
            if self.network.is_none() {
                missing.push("--network");
            }
            if self.amount.is_none() {
                missing.push("--amount");
            }
            if self.to.is_none() {
                missing.push("--to");
            }
        }
        if touches_chains {
            if self.rpc_url.is_none() {
                missing.push("--rpc-url (RPC_URL)");
            }
            if self.bridge_address.is_none() {
                missing.push("--bridge-address (BRIDGE_ADDRESS)");
            }
            if self.gql_endpoint.is_none() {
                missing.push("--gql-endpoint (BRIDGE_GQL_ENDPOINT)");
            }
            if self.usdc_bridge_account.is_none() {
                missing.push("--usdc-bridge-account (USDC_BRIDGE_ACCOUNT_ID)");
            }
            if self.deposit_prover_dir.is_none() {
                missing.push("--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR)");
            }
        }
        if starts_new
            && matches!(self.qr_mode, QrMode::Eip681 | QrMode::Both)
            && self.from_address.is_none()
        {
            missing.push("--from-address (required by --qr-mode eip681 and both)");
        }
        let wc_project_id = self
            .wc_project_id
            .clone()
            .or(DEFAULT_WC_PROJECT_ID.map(str::to_owned));
        if starts_new
            && matches!(self.qr_mode, QrMode::Walletconnect | QrMode::Both)
            && wc_project_id.is_none()
        {
            missing.push("--wc-project-id (this build has none compiled in)");
        }
        if !missing.is_empty() {
            return Err(CliError::Usage {
                reason: format!("deposit: missing {}", missing.join(", ")),
            });
        }

        let usdc_bridge_account = match &self.usdc_bridge_account {
            None => None,
            Some(raw) => {
                let mut out = [0u8; 32];
                if raw.len() != 64 || hex::decode_to_slice(raw, &mut out).is_err() {
                    return Err(CliError::ArgInvalid {
                        flag: "usdc-bridge-account",
                        expected: "64 hex characters".into(),
                        got: redact(raw),
                    });
                }
                Some(out)
            },
        };
        let home = || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        // Absolute from here on: the prover runs with its own working
        // directory, where a relative path would point somewhere else.
        let abs = |path: PathBuf, flag: &'static str| -> CliResult<PathBuf> {
            std::path::absolute(&path).map_err(|e| CliError::ArgInvalid {
                flag,
                expected: format!("a usable path ({e})"),
                got: redact(&path.to_string_lossy()),
            })
        };
        let secs = Duration::from_secs;
        Ok(DepositParams {
            mode,
            globals: *g,
            network: self.network,
            amount,
            to,
            rpc_url: self.rpc_url.clone(),
            bridge: self.bridge_address,
            gql_endpoint: self.gql_endpoint.clone(),
            usdc_bridge_account,
            prover_dir: self
                .deposit_prover_dir
                .clone()
                .map(|d| abs(d, "deposit-prover-dir"))
                .transpose()?,
            confirmations: self.confirmations,
            state_dir: abs(
                self.state_dir
                    .clone()
                    .unwrap_or_else(|| home().join(".bridge-deposit-state")),
                "state-dir",
            )?,
            work_dir: abs(
                self.work_dir
                    .clone()
                    .unwrap_or_else(|| home().join(".bridge-deposit-work")),
                "work-dir",
            )?,
            prover_timeout: secs(self.prover_timeout_s),
            anchor_timeout: (self.anchor_timeout_s > 0).then(|| secs(self.anchor_timeout_s)),
            relayer_grace: secs(self.relayer_grace_s),
            recovery_window: secs(self.recovery_window_s),
            credit_timeout: secs(self.credit_timeout_s),
            pair_timeout: secs(self.pair_timeout_s),
            qr_mode: self.qr_mode,
            qr_out: self.qr_out.clone().map(|q| abs(q, "qr-out")).transpose()?,
            uri_only: self.uri_only,
            qr_invert: self.qr_invert,
            wc_project_id,
            wc_relay_url: self.wc_relay_url.clone(),
            from_address: self.from_address,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    const ACC: &str = "a36247447a5112c5823e28dc904ee24b5447ee211eeb500d877f1b69c7d3e4a8";
    const DAPP: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn every_network_is_a_supported_deposit_chain() {
        for n in Network::ALL {
            assert!(
                deposit_chain_ids::SUPPORTED_DEPOSIT_CHAIN_IDS.contains(&n.chain_id()),
                "{n:?} ({}) is not a chain the deposit circuit accepts",
                n.chain_id()
            );
        }
        assert_eq!(Network::Sepolia.chain_id(), 11_155_111);
        assert_eq!(Network::Sepolia.caip2(), "eip155:11155111");
        assert_eq!(Network::Sepolia.eip3085()["chainId"], "0xaa36a7");
    }

    #[test]
    fn to_parses_dapp_and_account() {
        let t = AnTarget::parse(&format!("{DAPP}::{ACC}")).unwrap();
        assert_eq!(t.account_hex(), ACC);
        assert_eq!(t.dapp_hex(), DAPP);
        assert_eq!(t.extended(), format!("{DAPP}::{ACC}"));
    }

    #[test]
    fn to_lowercases_and_refuses_bad_shapes() {
        let up = format!("{}::{}", DAPP, ACC.to_uppercase());
        assert_eq!(AnTarget::parse(&up).unwrap().account_hex(), ACC);
        for bad in [
            ACC.to_string(),                       // no dapp
            format!("0:{ACC}"),                    // workchain form
            format!("0x{DAPP}::{ACC}"),            // 0x prefix
            format!("{DAPP}::{}", &ACC[..63]),     // short
            format!("{DAPP}::{}", "0".repeat(64)), // zero account
        ] {
            let e = AnTarget::parse(&bad).unwrap_err();
            assert_eq!(
                e.exit_code(),
                crate::errors::ExitCode::PreflightRefused,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_fresh_run_needs_network_amount_and_to() {
        let cli =
            crate::args::Cli::try_parse_from(["ackinacki-bridge", "deposit", "--amount", "1"])
                .unwrap();
        let crate::args::Command::Deposit(a) = cli.cmd else {
            panic!()
        };
        let e = a.validate(&GlobalFlags::default()).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("--network") && msg.contains("--to"), "{msg}");
    }

    #[test]
    fn an_amount_above_the_deposit_cap_is_refused() {
        let cli = crate::args::Cli::try_parse_from([
            "ackinacki-bridge",
            "deposit",
            "--network",
            "sepolia",
            "--amount",
            "18446744073709.551616",
            "--to",
            &format!("{DAPP}::{ACC}"),
        ])
        .unwrap();
        let crate::args::Command::Deposit(a) = cli.cmd else {
            panic!()
        };
        let e = a.validate(&GlobalFlags::default()).unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::PreflightRefused,
            "{e}"
        );
    }

    #[test]
    fn eip681_requires_from_address() {
        let cli = crate::args::Cli::try_parse_from([
            "ackinacki-bridge",
            "deposit",
            "--network",
            "sepolia",
            "--amount",
            "1",
            "--to",
            &format!("{DAPP}::{ACC}"),
            "--qr-mode",
            "eip681",
        ])
        .unwrap();
        let crate::args::Command::Deposit(a) = cli.cmd else {
            panic!()
        };
        let e = a.validate(&GlobalFlags::default()).unwrap_err();
        assert!(e.to_string().contains("--from-address"), "{e}");
    }

    #[test]
    fn relative_paths_become_absolute_before_anything_runs() {
        let cli = crate::args::Cli::try_parse_from([
            "ackinacki-bridge",
            "deposit",
            "--dry-run",
            "--network",
            "sepolia",
            "--amount",
            "1",
            "--to",
            &format!("{DAPP}::{ACC}"),
            "--rpc-url",
            "http://rpc.invalid",
            "--bridge-address",
            "0x0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7",
            "--gql-endpoint",
            "http://gql.invalid",
            "--usdc-bridge-account",
            &"1a".repeat(32),
            "--wc-project-id",
            "p",
            "--deposit-prover-dir",
            "./deposit-prover",
            "--work-dir",
            "work",
            "--state-dir",
            "./state",
        ])
        .unwrap();
        let crate::args::Command::Deposit(a) = cli.cmd else {
            panic!()
        };
        let p = a.validate(&GlobalFlags::default()).unwrap();
        let cwd = std::env::current_dir().unwrap();
        for (got, name) in [
            (p.prover_dir.unwrap(), "deposit-prover"),
            (p.work_dir, "work"),
            (p.state_dir, "state"),
        ] {
            assert!(
                got.is_absolute() && got.starts_with(&cwd) && got.ends_with(name),
                "{}",
                got.display()
            );
        }
    }

    #[test]
    fn resume_accepts_an_op_id_or_a_deposit_id() {
        assert!(matches!(
            OpRef::parse("01J9ZQ4X7T8V5N6M3K2P1R0S9A").unwrap(),
            OpRef::Op(_)
        ));
        assert_eq!(
            OpRef::parse("42").unwrap(),
            OpRef::DepositId(U256::from(42))
        );
        assert!(OpRef::parse("not-an-id").is_err());
    }

    #[test]
    fn the_acki_nacki_network_id_keeps_the_port() {
        // Two local nodes on one host are two networks.
        assert_eq!(
            an_network_id("http://localhost:8600/graphql").unwrap(),
            "http://localhost:8600"
        );
        assert_ne!(
            an_network_id("http://localhost:8600/graphql").unwrap(),
            an_network_id("http://localhost:8700/graphql").unwrap()
        );
        // No port is the scheme's default; case does not matter.
        assert_eq!(
            an_network_id("https://Shellnet.AckiNacki.org/graphql").unwrap(),
            "https://shellnet.ackinacki.org:443"
        );
        assert_eq!(
            an_network_id("https://shellnet.ackinacki.org:443/graphql").unwrap(),
            an_network_id("https://shellnet.ackinacki.org/graphql").unwrap()
        );
        assert!(an_network_id("ftp://shellnet.ackinacki.org").is_err());
    }
}
