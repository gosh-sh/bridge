//! Deposit relayer CLI (EVM→AN direction).
//!
//! Subcommands:
//!
//! - `watch` — read-only. Connects to an Ethereum RPC, reads the bridge's
//!   `depositCounter()`, and lists the confirmed `Deposit` events from a
//!   starting id. No proving, no AN side. Handy for confirming the relayer can
//!   see the deposits before running it in earnest.
//!
//! - `prove-one` — finds a single `depositId`, collects its block header and
//!   receipt proof, and writes them to `--out-dir` as `witness.json`: the
//!   `finalizeDeposit` arguments, without an AN node. Pass `--tx-hash` +
//!   `--log-index` to skip `eth_getLogs` and locate the deposit via receipt
//!   only.
//!
//! - `finalize-one` — sends the `witness.json` of a `prove-one` to
//!   `eccBridge.finalizeDeposit` on Acki Nacki.
//!
//! - `daemon` — long-running loop: listen → collect the receipt proof → submit,
//!   with exponential backoff and SIGINT/SIGTERM-aware shutdown. Live submit
//!   uses tvm_client 3.0 (`dapp_id::account_id` addresses); pass `--dry-run` to
//!   mock the AN side.
//!
//! - `status` — print the state file path.
//!
//! `daemon` writes only into the directories it is given (`--state-dir`,
//! `--log-dir`; bridge-audit#132), and `prove-one` only into `--out-dir`:
//! never the system temp directory, `$HOME` or the current directory. The
//! bridge ABI is built in; no ABI file is read.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use alloy::{primitives::Address, providers::ProviderBuilder};
use clap::{Parser, Subcommand};
use deposit_relayer_daemon::{
    bridge_abi, check_gas_credit, fetch_deposit_from_receipt, prepare_dir,
    refuse_unrecognised_state, resolve_from_block, AnConfig, AnInterfaceSubmitter, AnSubmitConfig,
    AnSubmitter, BackoffConfig, DeploymentIdentity, DepositSource, DepositWitness, EthLogSource,
    MockAnSubmitter, ReceiptWitnessBuilder, Relayer, RelayerConfig, RelayerMetrics, RelayerState,
    ServiceDirs, StateLock, SubmitOutcome, WitnessBuilder, BRIDGE_DEPLOY_BLOCK_ENV,
    LOG_FILE_PREFIX, STATE_FILE_NAME,
};
use tracing::{error, info, warn};
use tvm_client::crypto::KeyPair;

/// What `prove-one` writes into `--out-dir` and `finalize-one` reads from
/// `--bundle-dir`: the serialised `DepositWitness`.
const WITNESS_FILE_NAME: &str = "witness.json";

#[derive(Parser, Debug)]
#[command(
    name = "deposit-relayer",
    about = "EVM→Acki Nacki deposit bridge relayer: listen for Deposit events, collect their \
             receipt proofs, finalize on AN."
)]
struct Args {
    /// Removed: the state is a directory now. Kept hidden so an old command
    /// line or `DEPOSIT_RELAYER_STATE` in the environment fails with the
    /// migration hint instead of a bare "unexpected argument".
    #[arg(long, env = "DEPOSIT_RELAYER_STATE", hide = true, global = true)]
    state: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

/// The two directories of `daemon` (bridge-audit#132).
#[derive(clap::Args, Debug)]
struct DirArgs {
    /// Directory for `deposit-relayer-state.json`, its lock and the
    /// temporary file of its atomic write. Created when missing.
    #[arg(long, env = "DEPOSIT_RELAYER_STATE_DIR")]
    state_dir: PathBuf,
    /// Directory for the daemon's own log (one file per day, beside stdout).
    /// Created when missing.
    #[arg(long, env = "DEPOSIT_RELAYER_LOG_DIR")]
    log_dir: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Read-only: list confirmed Deposit events from `--start`.
    Watch {
        /// EVM JSON-RPC. Must serve `eth_getLogs` back to `--from-block`.
        #[arg(long, env = "RPC_URL", hide_env_values = true)]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        /// Lower bound for the log scan (bridge deploy block). When `0`, falls
        /// back to `BRIDGE_DEPLOY_BLOCK`.
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        /// Confirmation depth before a deposit is surfaced.
        #[arg(long, default_value_t = 12)]
        confirmations: u64,
        /// First depositId to list.
        #[arg(long, default_value_t = 0)]
        start: u64,
        /// How many deposit ids to probe.
        #[arg(long, default_value_t = 16)]
        count: u64,
        /// Optional expected `eth_chainId` (must be a supported deposit chain).
        /// When set, the CLI aborts if the RPC chain does not match.
        #[arg(long, env = "EXPECT_CHAIN_ID")]
        expect_chain_id: Option<u64>,
    },
    /// Find one depositId, write its `finalizeDeposit` arguments to
    /// `--out-dir/witness.json`.
    ProveOne {
        /// EVM JSON-RPC. Must serve `eth_getLogs`, transaction receipts and
        /// `eth_getBlockReceipts` for at least the last 7 days (30 preferred).
        #[arg(long, env = "RPC_URL", hide_env_values = true)]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long)]
        deposit_id: u64,
        /// Lower bound for the log scan (bridge deploy block). When `0`, falls
        /// back to `BRIDGE_DEPLOY_BLOCK`. Ignored when `--tx-hash` is set.
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        #[arg(long, default_value_t = 12)]
        confirmations: u64,
        /// Deposit transaction hash. With `--log-index`, skips `eth_getLogs`.
        #[arg(long, requires = "log_index")]
        tx_hash: Option<String>,
        /// Receipt-local log index (position in `receipt.logs[]`). Required
        /// with `--tx-hash`.
        #[arg(long, requires = "tx_hash")]
        log_index: Option<u64>,
        /// Where to write `witness.json`. Created when missing.
        #[arg(long)]
        out_dir: PathBuf,
        /// Optional expected `eth_chainId` (must be a supported deposit chain).
        #[arg(long, env = "EXPECT_CHAIN_ID")]
        expect_chain_id: Option<u64>,
    },
    /// Long-running listen→collect→submit loop.
    Daemon {
        #[command(flatten)]
        dirs: DirArgs,
        /// EVM JSON-RPC. Must serve `eth_getLogs`, transaction receipts and
        /// `eth_getBlockReceipts` for at least the last 7 days (30 preferred),
        /// and `eth_getLogs` back to `--from-block` on a start without a
        /// state file. One that keeps ~10 000 blocks cannot catch up after a
        /// stall.
        #[arg(long, env = "RPC_URL", hide_env_values = true)]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        /// Lower bound for the log scan (bridge deploy block). When `0`, falls
        /// back to `BRIDGE_DEPLOY_BLOCK`.
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        #[arg(long, default_value_t = 12)]
        confirmations: u64,
        /// First depositId to target on a fresh start.
        #[arg(long, default_value_t = 0)]
        start_deposit_id: u64,
        /// tvm_client 3.0 endpoint. The host must serve GraphQL *and*
        /// `/v2/messages` (`send_message`). A public gateway that only
        /// answers `/graphql` is not enough. Example:
        /// `http://127.0.0.1:11000/graphql`.
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: Option<String>,
        /// Path to tvm-cli keys JSON (signer for `finalizeDeposit`).
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: Option<String>,
        /// Bridge contract address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_TOKEN_BRIDGE")]
        an_token_bridge: Option<String>,
        /// Relayer signer address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_SENDER")]
        an_sender: Option<String>,
        /// Run the submit stage against an in-memory mock AN instead of
        /// tvm_client.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 5)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 120)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
        /// Accept a state file stamped for another deployment (chain or
        /// bridge) by starting it over: progress, scan cursor and parked
        /// ids are discarded and the daemon begins at `--start-deposit-id`.
        #[arg(long)]
        force_state: bool,
        /// After this many consecutive failures on one deposit, park it in
        /// `state.json` and stop retrying it (0 = disabled). Waiting for a
        /// deposit that is not made yet is not a failure. A failed deposit
        /// otherwise stays outstanding and is retried with backoff; it does
        /// not block later ids.
        #[arg(long, env = "SKIP_AFTER_ATTEMPTS", default_value_t = 0)]
        skip_after_attempts: u32,
        /// Base delay in seconds before retrying a failed deposit. Doubled
        /// on each later failure of that id, up to 64×. Anchor waits use the
        /// larger of that failure delay and age / 4 seconds (age component
        /// capped at 60). `0` disables both per-id delays.
        #[arg(long, env = "OUTSTANDING_BACKOFF_SECS", default_value_t = 2)]
        outstanding_backoff_secs: u64,
        /// Warn after this many seconds waiting for an AN block anchor.
        /// `0` warns immediately (default); repeats are limited to once per
        /// 15 minutes per deposit. Set a delay only when block coverage is
        /// monitored separately and anchor delivery latency is understood.
        #[arg(long, env = "ANCHOR_WAIT_WARN_AFTER_SECS", default_value_t = 0)]
        anchor_wait_warn_after_secs: u64,
        /// Allow non-HTTPS GraphQL endpoints for live submit (local dev only).
        #[arg(long)]
        allow_insecure_graphql: bool,
        /// Optional expected `eth_chainId`. The RPC's chain is checked against
        /// the deposit allowlist either way.
        #[arg(long, env = "EXPECT_CHAIN_ID")]
        expect_chain_id: Option<u64>,
        /// Bind address of the Prometheus text exporter (`GET /metrics`), e.g.
        /// `127.0.0.1:9467`. Unset = no exporter. Metric names are listed in
        /// `deposit_relayer_daemon::metrics`.
        #[arg(long, env = "DEPOSIT_RELAYER_METRICS_ADDR")]
        metrics_addr: Option<SocketAddr>,
    },
    /// Send the `witness.json` of a `prove-one` to `eccBridge.finalizeDeposit`
    /// on Acki Nacki, bypassing the Ethereum stages.
    ///
    /// This is the operator path for finalising a specific deposit — one the
    /// daemon parked after `--skip-after-attempts` — and the contract/node
    /// readiness self-test. The deposit's block must already be accepted by
    /// the AN bridge (exit 224 otherwise).
    FinalizeOne {
        /// Directory holding `witness.json` — a `prove-one --out-dir`.
        #[arg(long)]
        bundle_dir: PathBuf,
        /// tvm_client 3.0 endpoint. The host must serve GraphQL *and*
        /// `/v2/messages` (`send_message`). A public gateway that only
        /// answers `/graphql` is not enough. Example: shellnet
        /// `https://shellnet.ackinacki.org/graphql`.
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: String,
        /// Path to tvm-cli keys JSON (signer for `finalizeDeposit`).
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: String,
        /// Bridge contract address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_TOKEN_BRIDGE")]
        an_token_bridge: String,
        /// Relayer signer address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_SENDER")]
        an_sender: String,
    },
    /// Print the state file path and exit.
    Status {
        /// The daemon's state directory.
        #[arg(long, env = "DEPOSIT_RELAYER_STATE_DIR")]
        state_dir: PathBuf,
    },
}

/// Rust's default for an error out of `main` is `Error: {e:?}` on stderr,
/// unredacted: a transport error quotes the RPC URL, key included, and
/// under compose that line lands in the container log on every restart.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match real_main().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", redact_urls(&format!("{e:?}")));
            std::process::ExitCode::FAILURE
        },
    }
}

async fn real_main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(old) = args.state {
        // Before bridge-audit#132 the state was a file path. The lock and
        // the temporary file were derived from it; now they live in the
        // directory with it.
        anyhow::bail!(
            "--state / DEPOSIT_RELAYER_STATE ({}) is gone: the state is a directory now. Move the \
             file into a directory as {} and pass that directory as --state-dir / \
             DEPOSIT_RELAYER_STATE_DIR; see CHANGELOG.md.",
            old.display(),
            deposit_relayer_daemon::STATE_FILE_NAME,
        );
    }
    let log_dir = match &args.cmd {
        Cmd::Daemon {
            dirs, ..
        } => Some(dirs.log_dir.clone()),
        _ => None,
    };
    if let Some(dir) = &log_dir {
        prepare_dir("log", dir).map_err(|e| anyhow::anyhow!(e))?;
    }
    init_tracing(log_dir.as_deref()).map_err(|e| anyhow::anyhow!("log file: {e}"))?;

    match args.cmd {
        Cmd::Status {
            state_dir,
        } => {
            info!(
                "deposit-relayer state file = {}",
                state_dir
                    .join(deposit_relayer_daemon::STATE_FILE_NAME)
                    .display()
            );
            Ok(())
        },
        Cmd::Watch {
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start,
            count,
            expect_chain_id,
        } => watch(
            rpc_url,
            bridge_address,
            resolve_from_block(from_block),
            confirmations,
            start,
            count,
            expect_chain_id,
        )
        .await
        .map_err(log_err("watch")),
        Cmd::ProveOne {
            rpc_url,
            bridge_address,
            deposit_id,
            from_block,
            confirmations,
            tx_hash,
            log_index,
            out_dir,
            expect_chain_id,
        } => prove_one(
            rpc_url,
            bridge_address,
            deposit_id,
            resolve_from_block(from_block),
            confirmations,
            tx_hash,
            log_index,
            out_dir,
            expect_chain_id,
        )
        .await
        .map_err(log_err("prove-one")),
        Cmd::FinalizeOne {
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_token_bridge,
            an_sender,
        } => finalize_one(
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_token_bridge,
            an_sender,
        )
        .await
        .map_err(log_err("finalize-one")),
        Cmd::Daemon {
            dirs,
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start_deposit_id,
            an_graphql_url,
            an_keys_path,
            an_token_bridge,
            an_sender,
            dry_run,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            force_state,
            skip_after_attempts,
            outstanding_backoff_secs,
            anchor_wait_warn_after_secs,
            allow_insecure_graphql,
            expect_chain_id,
            metrics_addr,
        } => {
            if let Some(addr) = metrics_addr {
                install_metrics_exporter(addr)?;
            }
            let service_dirs = ServiceDirs {
                state: dirs.state_dir,
                log: dirs.log_dir,
            };
            service_dirs.prepare().map_err(|e| anyhow::anyhow!(e))?;
            info!(
                state_dir = %service_dirs.state.display(),
                log_dir = %service_dirs.log.display(),
                "service directories",
            );
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            backoff.validate().map_err(|e| anyhow::anyhow!(e))?;
            let mut an_cfg = AnConfig::default();
            if let Some(url) = an_graphql_url {
                an_cfg.graphql_url = url;
            }
            if let Some(path) = an_keys_path {
                an_cfg.keys_path = path;
            }
            if let Some(addr) = an_token_bridge {
                an_cfg.token_bridge = addr;
            }
            if let Some(addr) = an_sender {
                an_cfg.sender = addr;
            }
            run_daemon(
                service_dirs.state_file(),
                rpc_url,
                bridge_address,
                resolve_from_block(from_block),
                confirmations,
                start_deposit_id,
                an_cfg,
                dry_run,
                backoff,
                force_state,
                skip_after_attempts,
                outstanding_backoff_secs,
                anchor_wait_warn_after_secs,
                allow_insecure_graphql,
                expect_chain_id,
            )
            .await
            .map_err(log_err("daemon"))
        },
    }
}

async fn finalize_one(
    bundle_dir: PathBuf,
    an_graphql_url: String,
    an_keys_path: String,
    an_token_bridge: String,
    an_sender: String,
) -> anyhow::Result<()> {
    let path = bundle_dir.join(WITNESS_FILE_NAME);
    let raw = std::fs::read(&path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let witness: DepositWitness = serde_json::from_slice(&raw)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    check_gas_credit(&witness)?;
    info!(
        chain_id = witness.chain_id,
        block_number = witness.block_number,
        block_hash = %witness.block_hash,
        tx_index = witness.tx_index,
        log_index = witness.log_index,
        payload_len = witness.payload_len(),
        "submitting finalizeDeposit(chainId, header, txIndex, receiptProof, logIndex) to AN",
    );

    let keys_json = std::fs::read_to_string(&an_keys_path)?;
    let keys: KeyPair = serde_json::from_str(&keys_json)?;
    let tvm = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi: bridge_abi(),
    })
    .map_err(|e| anyhow::anyhow!("connect tvm_client: {e}"))?;

    let submit_cfg = AnSubmitConfig {
        from: an_sender,
        token_bridge: an_token_bridge,
        confirm_timeout_secs: 60,
    };
    let submitter = AnInterfaceSubmitter::new(Arc::new(tvm), submit_cfg);

    match submitter.submit_witness(&witness).await? {
        SubmitOutcome::Finalized {
            tx_hash,
        } => {
            info!(
                tx = tx_hash.map(hex::encode).unwrap_or_default(),
                "finalizeDeposit ACCEPTED — deposit finalised on AN",
            );
            Ok(())
        },
        SubmitOutcome::AlreadyFinalized => {
            info!("deposit already finalised on AN (its voucher exists)");
            Ok(())
        },
        SubmitOutcome::Rejected {
            reason,
        } => anyhow::bail!("finalizeDeposit rejected by AN: {reason}"),
        SubmitOutcome::Pending {
            reason,
        } => anyhow::bail!("finalizeDeposit still pending on AN: {reason}"),
    }
}

async fn ensure_supported_rpc_chain(
    provider: &impl alloy::providers::Provider,
    expect_chain_id: Option<u64>,
) -> anyhow::Result<u64> {
    use deposit_relayer_daemon::{is_supported_deposit_chain, supported_deposit_chains_display};
    let id = provider.get_chain_id().await?;
    if !is_supported_deposit_chain(id) {
        anyhow::bail!(
            "RPC eth_chainId {id} is not a supported deposit chain; supported: {}",
            supported_deposit_chains_display()
        );
    }
    if let Some(expected) = expect_chain_id {
        if id != expected {
            anyhow::bail!("RPC eth_chainId {id} != --expect-chain-id {expected}");
        }
    }
    Ok(id)
}

async fn watch(
    rpc_url: String,
    bridge_address: Address,
    from_block: u64,
    confirmations: u64,
    start: u64,
    count: u64,
    expect_chain_id: Option<u64>,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = ensure_supported_rpc_chain(&provider, expect_chain_id).await?;
    let source = EthLogSource::new(provider, bridge_address, from_block, confirmations);

    let counter = source.deposit_counter().await?;
    info!(%bridge_address, chain_id, deposit_counter = %counter, "bridge state");

    for id in start..start.saturating_add(count) {
        match source.fetch(id).await? {
            Some(ev) => info!(
                deposit_id = ev.deposit_id,
                sender = %ev.sender,
                amount = %ev.amount,
                tx = %ev.tx_hash,
                block = ev.block_number,
                "confirmed deposit",
            ),
            None => info!(deposit_id = id, "not visible / not yet confirmed"),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn prove_one(
    rpc_url: String,
    bridge_address: Address,
    deposit_id: u64,
    from_block: u64,
    confirmations: u64,
    tx_hash: Option<String>,
    log_index: Option<u64>,
    out_dir: PathBuf,
    expect_chain_id: Option<u64>,
) -> anyhow::Result<()> {
    if tx_hash.is_none() && from_block == 0 {
        warn!(
            "from_block is 0 and no --tx-hash supplied; eth_getLogs will scan from genesis. Set \
             {BRIDGE_DEPLOY_BLOCK_ENV} or pass --from-block to the bridge deploy block.",
        );
    }

    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    ensure_supported_rpc_chain(&provider, expect_chain_id).await?;

    let event = if let (Some(tx_hash), Some(log_index)) = (tx_hash, log_index) {
        let tx_hash = alloy::primitives::B256::from_str(&tx_hash)
            .map_err(|e| anyhow::anyhow!("invalid --tx-hash: {e}"))?;
        info!(
            deposit_id,
            tx = %tx_hash,
            log_index,
            "fast path: locating deposit via receipt (skipping eth_getLogs)",
        );
        match fetch_deposit_from_receipt(
            &provider,
            bridge_address,
            tx_hash,
            log_index,
            deposit_id,
            confirmations,
        )
        .await?
        {
            Some(e) => e,
            None => {
                anyhow::bail!(
                    "depositId {deposit_id} in tx {tx_hash:#x} not confirmed yet (needs \
                     {confirmations} confirmations)"
                );
            },
        }
    } else {
        let source = EthLogSource::new(provider.clone(), bridge_address, from_block, confirmations);
        match source.fetch(deposit_id).await? {
            Some(e) => e,
            None => {
                anyhow::bail!("depositId {deposit_id} not visible / not confirmed yet");
            },
        }
    };
    info!(
        deposit_id = event.deposit_id,
        tx = %event.tx_hash,
        log_index = event.log_index,
        "found deposit; collecting its block header and receipt proof",
    );

    let witness = ReceiptWitnessBuilder::new(provider, bridge_address)
        .build(&event)
        .await?;
    check_gas_credit(&witness)?;

    prepare_dir("out", &out_dir).map_err(|e| anyhow::anyhow!(e))?;
    let path = out_dir.join(WITNESS_FILE_NAME);
    std::fs::write(&path, serde_json::to_vec_pretty(&witness)?)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    info!(
        file = %path.display(),
        chain_id = witness.chain_id,
        block_number = witness.block_number,
        block_hash = %witness.block_hash,
        tx_index = witness.tx_index,
        log_index = witness.log_index,
        payload_len = witness.payload_len(),
        "wrote the finalizeDeposit arguments",
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon(
    state_path: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    from_block: u64,
    confirmations: u64,
    start_deposit_id: u64,
    an_cfg: AnConfig,
    dry_run: bool,
    backoff: BackoffConfig,
    force_state: bool,
    skip_after_attempts: u32,
    outstanding_backoff_secs: u64,
    anchor_wait_warn_after_secs: u64,
    allow_insecure_graphql: bool,
    expect_chain_id: Option<u64>,
) -> anyhow::Result<()> {
    refuse_unrecognised_state(&state_path, STATE_FILE_NAME)?;
    let _state_lock = StateLock::acquire(&state_path)
        .map_err(|e| anyhow::anyhow!("failed to acquire state lock: {e}"))?;

    let existing_state = RelayerState::load(&state_path)?.unwrap_or_default();
    let scan_cursor = Arc::new(Mutex::new(existing_state.scan_done_through_block));

    // Same gate as `watch` / `prove-one`: the AN-side bridge has no allowlist
    // entry for an unsupported chain, so fail before the first deposit.
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = ensure_supported_rpc_chain(&provider, expect_chain_id).await?;
    info!(
        chain_id,
        chain = deposit_relayer_daemon::supported_deposit_chain_name(chain_id).unwrap_or("?"),
        "deposit source chain",
    );
    let deployment = DeploymentIdentity::new(chain_id, bridge_address);

    let witness = Arc::new(ReceiptWitnessBuilder::new(provider.clone(), bridge_address));
    let source = Arc::new(
        EthLogSource::new(provider, bridge_address, from_block, confirmations)
            .with_scan_cursor(scan_cursor.clone()),
    );
    deposit_relayer_daemon::metrics::set_build_info();
    spawn_deposit_counter_poller(source.clone());

    let skip_after = if skip_after_attempts == 0 {
        None
    } else {
        Some(skip_after_attempts)
    };

    if dry_run {
        warn!(
            "running in --dry-run mode: receipt proofs are built from the real chain but deposits \
             are 'finalized' only in an in-memory mock AN"
        );
        run_daemon_loop(
            state_path,
            start_deposit_id,
            backoff,
            deployment,
            force_state,
            skip_after,
            outstanding_backoff_secs,
            anchor_wait_warn_after_secs,
            scan_cursor,
            source,
            witness,
            Arc::new(MockAnSubmitter::accepting()),
        )
        .await
    } else {
        if !an_cfg.is_live_submit_ready() {
            anyhow::bail!(
                "live submit requires --an-graphql-url, --an-keys-path, --an-token-bridge and \
                 --an-sender (addresses in dapp_id::account_id form). Or pass --dry-run to \
                 exercise the Ethereum side only."
            );
        }
        an_cfg
            .validate_live_graphql_endpoint(allow_insecure_graphql)
            .map_err(|e| anyhow::anyhow!(e))?;
        if !allow_insecure_graphql && an_cfg.graphql_url.starts_with("http://") {
            warn!(
                graphql_url = %an_cfg.graphql_url,
                "GraphQL uses HTTP on loopback or dev; use HTTPS in production",
            );
        }
        let keys_json = std::fs::read_to_string(&an_cfg.keys_path)?;
        let keys: KeyPair = serde_json::from_str(&keys_json)?;
        let tvm = TvmAckiNacki::connect(TvmClientConfig {
            graphql_endpoints: vec![an_cfg.graphql_url.clone()],
            keys,
            bridge_abi: bridge_abi(),
        })
        .map_err(|e| anyhow::anyhow!("connect tvm_client: {e}"))?;
        let submit_cfg = an_cfg.to_submit_config();
        let submitter =
            AnInterfaceSubmitter::new(Arc::new(tvm), submit_cfg).with_voucher_check(bridge_abi())?;
        run_daemon_loop(
            state_path,
            start_deposit_id,
            backoff,
            deployment,
            force_state,
            skip_after,
            outstanding_backoff_secs,
            anchor_wait_warn_after_secs,
            scan_cursor,
            source,
            witness,
            Arc::new(submitter),
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_loop<S, W, A>(
    state_path: PathBuf,
    start_deposit_id: u64,
    backoff: BackoffConfig,
    deployment: DeploymentIdentity,
    force_state: bool,
    skip_after_attempts: Option<u32>,
    outstanding_backoff_secs: u64,
    anchor_wait_warn_after_secs: u64,
    scan_cursor: Arc<Mutex<Option<u64>>>,
    source: Arc<S>,
    witness: Arc<W>,
    submitter: Arc<A>,
) -> anyhow::Result<()>
where
    S: DepositSource,
    W: WitnessBuilder,
    A: AnSubmitter,
{
    let cfg = RelayerConfig {
        state_path,
        start_deposit_id,
        poll_interval: backoff.initial,
        deployment: Some(deployment),
        force_state,
        skip_after_attempts,
        outstanding_backoff_secs,
        anchor_wait_warn_after_secs,
        scan_cursor: Some(scan_cursor),
    };
    let mut relayer = Relayer::new(cfg, source, witness, submitter)?;
    let metrics = RelayerMetrics::new();

    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(?e, "failed to install SIGTERM handler; SIGINT only");
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                },
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("SIGINT received"),
                _ = sigterm.recv() => info!("SIGTERM received"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("SIGINT received");
        }
    };

    info!(?backoff, "deposit daemon starting");
    let summary = relayer
        .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
        .await?;
    info!(?summary, snapshot = ?metrics.snapshot(), "deposit daemon stopped");
    Ok(())
}

/// Serve the Prometheus text format at `http://<addr>/metrics` for the
/// lifetime of the process. Every `metrics::*` macro in this binary and in
/// `deposit_relayer_daemon` records into it. Same buckets as the AN→ETH
/// relayer, so one scrape config fits both.
fn install_metrics_exporter(addr: SocketAddr) -> anyhow::Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;

    const BUCKETS: &[f64] = &[
        0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    ];
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .set_buckets(BUCKETS)
        .map_err(|e| anyhow::anyhow!("metrics exporter buckets: {e}"))?
        .install()
        .map_err(|e| anyhow::anyhow!("metrics exporter on {addr}: {e}"))?;
    deposit_relayer_daemon::metrics::describe();
    info!(%addr, "metrics exporter listening (GET /metrics)");
    Ok(())
}

/// How often the daemon reads `depositCounter()` for the backlog gauge.
const DEPOSIT_COUNTER_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Keep `deposit_relayer_eth_deposit_counter` fresh: one `eth_call` a minute,
/// independent of the tick loop, so the backlog stays visible during long
/// retries.
fn spawn_deposit_counter_poller<P>(source: Arc<EthLogSource<P>>)
where
    P: alloy::providers::Provider<alloy::network::Ethereum> + 'static,
{
    tokio::spawn(async move {
        loop {
            match source.deposit_counter().await {
                Ok(counter) => {
                    let value = u64::try_from(counter).unwrap_or(u64::MAX);
                    metrics::gauge!(deposit_relayer_daemon::metrics::ETH_DEPOSIT_COUNTER)
                        .set(value as f64);
                },
                Err(e) => warn!(error = %e, "depositCounter() poll failed"),
            }
            tokio::time::sleep(DEPOSIT_COUNTER_POLL_INTERVAL).await;
        }
    });
}

fn log_err(stage: &'static str) -> impl Fn(anyhow::Error) -> anyhow::Error {
    move |e| {
        error!(stage, ?e, "command failed");
        e
    }
}

use deposit_relayer_daemon::redact::{redact_urls, Redacting};

/// Daily files kept in the log directory before the oldest is deleted.
const LOG_KEEP_FILES: usize = 14;

/// stdout always; with a log directory also one file per day there
/// (`LOG_FILE_PREFIX.<date>`), written synchronously so nothing is lost when
/// the process exits through `std::process::exit`.
fn init_tracing(log_dir: Option<&Path>) -> std::io::Result<()> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Both sinks redact URLs: transport errors quote the RPC URL, key included.
    let stdout = fmt::layer().with_writer(Redacting(std::io::stdout));
    match log_dir {
        Some(dir) => {
            let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix(LOG_FILE_PREFIX)
                .max_log_files(LOG_KEEP_FILES)
                .build(dir)
                .map_err(std::io::Error::other)?;
            let file = fmt::layer()
                .with_ansi(false)
                .with_writer(Redacting(file_appender));
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stdout)
                .with(file)
                .try_init();
        },
        None => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stdout)
                .try_init();
        },
    }
    Ok(())
}
