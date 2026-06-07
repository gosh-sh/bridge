//! Deposit relayer CLI (EVM→AN direction).
//!
//! Subcommands:
//!
//! - `watch` — read-only. Connects to an Ethereum RPC, reads the bridge's
//!   `depositCounter()`, and lists the confirmed `Deposit` events from a
//!   starting id. No proving, no AN side. Handy for confirming the relayer can
//!   see the deposits before running it in earnest.
//!
//! - `prove-one` — listens for a single `depositId`, runs the `deposit-prover`
//!   pipeline out-of-process, and writes the three opcode operands (`vk_blob`,
//!   `public_inputs`, `proof`) to an output directory. Exercises the full
//!   listen→prove path without an AN node.
//!
//! - `daemon` — long-running loop: listen → prove → submit, with exponential
//!   backoff and SIGINT/SIGTERM-aware shutdown. Live submit uses tvm_client 3.0
//!   (`dapp_id::account_id` addresses); pass `--dry-run` to mock the AN side.
//!
//! - `status` — print the state file path.

use std::{path::PathBuf, sync::Arc, time::Duration};

use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use alloy::{primitives::Address, providers::ProviderBuilder};
use clap::{Parser, Subcommand};
use deposit_relayer_daemon::{
    AnConfig, AnInterfaceSubmitter, AnSubmitter, BackoffConfig, DepositSource, EthLogSource,
    MockAnSubmitter, ProofGenerator, Relayer, RelayerConfig, RelayerMetrics,
    SubprocessProofGenerator, SubprocessProverConfig, DEFAULT_AN_NODE_URL,
};
use tracing::{error, info, warn};
use tvm_client::crypto::KeyPair;

#[derive(Parser, Debug)]
#[command(
    name = "deposit-relayer",
    about = "EVM→Acki Nacki deposit bridge relayer: listen for Deposit events, prove them, \
             finalize on AN."
)]
struct Args {
    /// Where to persist `state.json` (used by `daemon`).
    #[arg(long, default_value = "./deposit-relayer-state.json")]
    state: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Read-only: list confirmed Deposit events from `--start`.
    Watch {
        #[arg(long)]
        rpc_url: String,
        #[arg(long)]
        bridge_address: Address,
        /// Lower bound for the log scan (bridge deploy block).
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
    },
    /// Listen for one depositId, prove it, write the operands to `--out-dir`.
    ProveOne {
        #[arg(long)]
        rpc_url: String,
        #[arg(long)]
        bridge_address: Address,
        #[arg(long)]
        deposit_id: u64,
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        #[arg(long, default_value_t = 12)]
        confirmations: u64,
        /// Path to the `deposit-prover` crate root.
        #[arg(long)]
        deposit_prover_dir: PathBuf,
        /// RPC URL passed to `deposit-prover` (defaults to `--rpc-url`).
        #[arg(long)]
        prover_rpc_url: Option<String>,
        #[arg(long, default_value_t = 18)]
        degree: u32,
        #[arg(long, default_value_t = 256)]
        max_data_byte_len: usize,
        #[arg(long, default_value_t = 20)]
        max_log_num: usize,
        /// Acki Nacki destination dApp identifier (UInt256), hex. Config tag
        /// bound as the dappId public inputs (not part of the deposit event).
        #[arg(long, default_value = "0")]
        dapp_id: String,
        /// Where to write `vk_blob.bin` / `public_inputs.bin` / `proof.bin`.
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Long-running listen→prove→submit loop.
    Daemon {
        #[arg(long)]
        rpc_url: String,
        #[arg(long)]
        bridge_address: Address,
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        #[arg(long, default_value_t = 12)]
        confirmations: u64,
        /// First depositId to target on a fresh start.
        #[arg(long, default_value_t = 0)]
        start_deposit_id: u64,
        #[arg(long)]
        deposit_prover_dir: PathBuf,
        #[arg(long)]
        prover_rpc_url: Option<String>,
        #[arg(long, default_value_t = 18)]
        degree: u32,
        #[arg(long, default_value_t = 256)]
        max_data_byte_len: usize,
        #[arg(long, default_value_t = 20)]
        max_log_num: usize,
        /// Acki Nacki destination dApp identifier (UInt256), hex. Config tag
        /// bound as the dappId public inputs (not part of the deposit event).
        #[arg(long, default_value = "0")]
        dapp_id: String,
        /// AN node REST base URL for BK-set preflight (`/v2/bk_set`).
        #[arg(long, env = "AN_NODE_URL")]
        an_node_url: Option<String>,
        /// GraphQL endpoint for tvm_client 3.0 (live submit). Example:
        /// `http://127.0.0.1:11000/graphql`.
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: Option<String>,
        /// Path to tvm-cli keys JSON (signer for `finalizeDeposit`).
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: Option<String>,
        /// Path to `USDCBridge.abi.json` on the AN side.
        #[arg(long, env = "AN_BRIDGE_ABI_PATH")]
        an_bridge_abi_path: Option<String>,
        /// Bridge contract address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_TOKEN_BRIDGE")]
        an_token_bridge: Option<String>,
        /// Relayer signer address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_SENDER")]
        an_sender: Option<String>,
        /// ECC token id for `finalizeDeposit`.
        #[arg(long, default_value_t = 1)]
        an_token_id: u32,
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
    },
    /// Probe the AN node's read endpoints (`/v2/bk_set`) and print the
    /// current BK-set summary. Confirms an AN config points at a reachable
    /// node before running the daemon.
    AnPreflight {
        /// AN node REST base URL.
        #[arg(long, env = "AN_NODE_URL", default_value = DEFAULT_AN_NODE_URL)]
        an_node_url: String,
    },
    /// Print the state file path and exit.
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = Args::parse();

    match args.cmd {
        Cmd::Status => {
            info!("deposit-relayer state file = {}", args.state.display());
            Ok(())
        },
        Cmd::Watch {
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start,
            count,
        } => watch(
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start,
            count,
        )
        .await
        .map_err(log_err("watch")),
        Cmd::ProveOne {
            rpc_url,
            bridge_address,
            deposit_id,
            from_block,
            confirmations,
            deposit_prover_dir,
            prover_rpc_url,
            degree,
            max_data_byte_len,
            max_log_num,
            dapp_id,
            out_dir,
        } => {
            let prover_cfg = build_prover_cfg(
                deposit_prover_dir,
                prover_rpc_url.unwrap_or_else(|| rpc_url.clone()),
                degree,
                max_data_byte_len,
                max_log_num,
                dapp_id,
            );
            prove_one(
                rpc_url,
                bridge_address,
                deposit_id,
                from_block,
                confirmations,
                prover_cfg,
                out_dir,
            )
            .await
            .map_err(log_err("prove-one"))
        },
        Cmd::AnPreflight {
            an_node_url,
        } => an_preflight(an_node_url)
            .await
            .map_err(log_err("an-preflight")),
        Cmd::Daemon {
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start_deposit_id,
            deposit_prover_dir,
            prover_rpc_url,
            degree,
            max_data_byte_len,
            max_log_num,
            dapp_id,
            an_node_url,
            an_graphql_url,
            an_keys_path,
            an_bridge_abi_path,
            an_token_bridge,
            an_sender,
            an_token_id,
            dry_run,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
        } => {
            let prover_cfg = build_prover_cfg(
                deposit_prover_dir,
                prover_rpc_url.unwrap_or_else(|| rpc_url.clone()),
                degree,
                max_data_byte_len,
                max_log_num,
                dapp_id,
            );
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            let mut an_cfg = AnConfig::default();
            if let Some(url) = an_node_url {
                an_cfg.node_url = url;
            }
            if let Some(url) = an_graphql_url {
                an_cfg.graphql_url = url;
            }
            if let Some(path) = an_keys_path {
                an_cfg.keys_path = path;
            }
            if let Some(path) = an_bridge_abi_path {
                an_cfg.bridge_abi_path = path;
            }
            if let Some(addr) = an_token_bridge {
                an_cfg.token_bridge = addr;
            }
            if let Some(addr) = an_sender {
                an_cfg.sender = addr;
            }
            an_cfg.token_id = an_token_id;
            run_daemon(
                args.state,
                rpc_url,
                bridge_address,
                from_block,
                confirmations,
                start_deposit_id,
                prover_cfg,
                an_cfg,
                dry_run,
                backoff,
            )
            .await
            .map_err(log_err("daemon"))
        },
    }
}

async fn an_preflight(an_node_url: String) -> anyhow::Result<()> {
    let cfg = AnConfig::from_node_url(an_node_url);
    info!(node_url = %cfg.node_url, "probing AN node /v2/bk_set");
    let pf = cfg.preflight().await?;
    info!(
        node_url = %pf.node_url,
        seq_no = pf.seq_no,
        bk_count = pf.bk_count,
        future_bk_count = pf.future_bk_count,
        "AN node reachable",
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_prover_cfg(
    deposit_prover_dir: PathBuf,
    rpc_url: String,
    degree: u32,
    max_data_byte_len: usize,
    max_log_num: usize,
    dapp_id: String,
) -> SubprocessProverConfig {
    let mut cfg = SubprocessProverConfig::new(deposit_prover_dir, rpc_url);
    cfg.degree = degree;
    cfg.max_data_byte_len = max_data_byte_len;
    cfg.max_log_num = max_log_num;
    cfg.dapp_id = dapp_id;
    cfg
}

async fn watch(
    rpc_url: String,
    bridge_address: Address,
    from_block: u64,
    confirmations: u64,
    start: u64,
    count: u64,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let source = EthLogSource::new(provider, bridge_address, from_block, confirmations);

    let counter = source.deposit_counter().await?;
    info!(%bridge_address, deposit_counter = %counter, "bridge state");

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
    prover_cfg: SubprocessProverConfig,
    out_dir: PathBuf,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let source = EthLogSource::new(provider, bridge_address, from_block, confirmations);

    let event = match source.fetch(deposit_id).await? {
        Some(e) => e,
        None => {
            anyhow::bail!("depositId {deposit_id} not visible / not confirmed yet");
        },
    };
    info!(
        deposit_id = event.deposit_id,
        tx = %event.tx_hash,
        log_index = event.log_index,
        "found deposit; generating proof (this runs the halo2 prover out-of-process)",
    );

    let prover = SubprocessProofGenerator::new(prover_cfg);
    let bundle = prover.generate(&event).await?;

    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join("vk_blob.bin"), &bundle.vk_blob)?;
    std::fs::write(out_dir.join("public_inputs.bin"), &bundle.public_inputs)?;
    std::fs::write(out_dir.join("proof.bin"), &bundle.proof)?;

    info!(
        out_dir = %out_dir.display(),
        vk_blob_len = bundle.vk_blob.len(),
        public_inputs_len = bundle.public_inputs.len(),
        proof_len = bundle.proof.len(),
        "wrote operands",
    );
    info!(
        deposit_id = %bundle.parsed.deposit_id,
        sender = %bundle.parsed.sender,
        amount = %bundle.parsed.amount,
        contract_address = %bundle.parsed.contract_address,
        block_hash_high = %bundle.parsed.block_hash_high,
        block_hash_low = %bundle.parsed.block_hash_low,
        promise_commit = %bundle.parsed.promise_commit,
        "public inputs",
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
    prover_cfg: SubprocessProverConfig,
    an_cfg: AnConfig,
    dry_run: bool,
    backoff: BackoffConfig,
) -> anyhow::Result<()> {
    if !an_cfg.node_url.is_empty() {
        let pf = an_cfg.preflight().await?;
        info!(
            node_url = %pf.node_url,
            seq_no = pf.seq_no,
            bk_count = pf.bk_count,
            "AN node preflight OK",
        );
    } else {
        warn!("no AN node_url configured; skipping /v2/bk_set preflight");
    }

    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let source = Arc::new(EthLogSource::new(
        provider,
        bridge_address,
        from_block,
        confirmations,
    ));
    let prover = Arc::new(SubprocessProofGenerator::new(prover_cfg));

    if dry_run {
        warn!(
            "running in --dry-run mode: deposits are proven against the real chain but \
             'finalized' only in an in-memory mock AN"
        );
        run_daemon_loop(
            state_path,
            start_deposit_id,
            backoff,
            source,
            prover,
            Arc::new(MockAnSubmitter::accepting()),
        )
        .await
    } else {
        if !an_cfg.is_live_submit_ready() {
            anyhow::bail!(
                "live submit requires --an-graphql-url, --an-keys-path, --an-bridge-abi-path, \
                 --an-token-bridge, and --an-sender (all in dapp_id::account_id form). Or pass \
                 --dry-run to exercise listen→prove only."
            );
        }
        let keys_json = std::fs::read_to_string(&an_cfg.keys_path)?;
        let keys: KeyPair = serde_json::from_str(&keys_json)?;
        let bridge_abi = TvmAckiNacki::load_abi(&an_cfg.bridge_abi_path)
            .map_err(|e| anyhow::anyhow!("load bridge ABI: {e}"))?;
        let tvm = TvmAckiNacki::connect(TvmClientConfig {
            graphql_endpoints: vec![an_cfg.graphql_url.clone()],
            keys,
            bridge_abi,
        })
        .map_err(|e| anyhow::anyhow!("connect tvm_client: {e}"))?;
        if tvm.supports_dapp_id().await? {
            info!("AN node supports SDK 3.0 dapp_id wire format");
        } else {
            warn!("AN node is pre-1.0.0; empty dapp_id is allowed on the wire");
        }
        let submit_cfg = an_cfg.to_submit_config();
        run_daemon_loop(
            state_path,
            start_deposit_id,
            backoff,
            source,
            prover,
            Arc::new(AnInterfaceSubmitter::new(Arc::new(tvm), submit_cfg)),
        )
        .await
    }
}

async fn run_daemon_loop<S, P, A>(
    state_path: PathBuf,
    start_deposit_id: u64,
    backoff: BackoffConfig,
    source: Arc<S>,
    prover: Arc<P>,
    submitter: Arc<A>,
) -> anyhow::Result<()>
where
    S: DepositSource,
    P: ProofGenerator,
    A: AnSubmitter,
{
    let cfg = RelayerConfig {
        state_path,
        start_deposit_id,
        poll_interval: backoff.initial,
        max_attempts_warn: 16,
    };
    let mut relayer = Relayer::new(cfg, source, prover, submitter)?;
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

fn log_err(stage: &'static str) -> impl Fn(anyhow::Error) -> anyhow::Error {
    move |e| {
        error!(stage, ?e, "command failed");
        e
    }
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}
