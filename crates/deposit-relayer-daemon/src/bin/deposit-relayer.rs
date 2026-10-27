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
//!   listen→prove path without an AN node. Pass `--tx-hash` + `--log-index` to
//!   skip `eth_getLogs` and locate the deposit via receipt only.
//!
//! - `daemon` — long-running loop: listen → prove → submit, with exponential
//!   backoff and SIGINT/SIGTERM-aware shutdown. Live submit uses tvm_client 3.0
//!   (`dapp_id::account_id` addresses); pass `--dry-run` to mock the AN side.
//!
//! - `arbitrum-anchors` — long-running loop next to `daemon` for a bridge on an
//!   Arbitrum chain: follows batches and assertions on the parent chain and
//!   calls `ArbitrumBlockVerifier.proveBlock` on Acki Nacki, so the blocks that
//!   hold deposits reach the anchor set without the owner.
//!
//! - `arbitrum-vector` — writes one `proveBlock` vector from the live chains
//!   the way `arbitrum-anchors` builds its calls, for the bench and the
//!   local-network test. Sends nothing.
//!
//! - `status` — print the state file path.

use std::{
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use alloy::{
    primitives::Address,
    providers::{Provider, ProviderBuilder},
};
use clap::{Parser, Subcommand};
use deposit_relayer_daemon::{
    arbitrum, fetch_deposit_from_receipt, parse_and_validate_dapp_id, resolve_from_block, AnConfig,
    AnInterfaceSubmitter, AnSubmitConfig, AnSubmitter, BackoffConfig, DeploymentIdentity,
    DepositProofBundle, DepositProverBins, DepositSource, EthLogSource, MockAnSubmitter,
    ProofGenerator, Relayer, RelayerConfig, RelayerMetrics, RelayerState, StateLock, SubmitOutcome,
    SubprocessProofGenerator, SubprocessProverConfig, BRIDGE_DEPLOY_BLOCK_ENV,
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
    #[arg(
        long,
        env = "DEPOSIT_RELAYER_STATE",
        default_value = "./deposit-relayer-state.json"
    )]
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
        #[arg(long)]
        expect_chain_id: Option<u64>,
    },
    /// Listen for one depositId, prove it, write the operands to `--out-dir`.
    ProveOne {
        #[arg(long)]
        rpc_url: String,
        #[arg(long)]
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
        /// Working directory of the `deposit-prover` examples: the proving
        /// key under `data/` and `data/kzg_params_18.srs` are read there.
        #[arg(long, env = "DEPOSIT_PROVER_DIR")]
        deposit_prover_dir: PathBuf,
        /// Prebuilt `fetch_deposit_data` example (`cargo build --release
        /// --examples` in `deposit-prover/`). Checked to exist and be
        /// executable at startup.
        #[arg(long, env = "DEPOSIT_FETCH_DEPOSIT_DATA_BIN")]
        fetch_deposit_data_bin: PathBuf,
        /// Prebuilt `export_vk_blob` example.
        #[arg(long, env = "DEPOSIT_EXPORT_VK_BLOB_BIN")]
        export_vk_blob_bin: PathBuf,
        /// Prebuilt `export_blake2b_proof` example.
        #[arg(long, env = "DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN")]
        export_blake2b_proof_bin: PathBuf,
        /// RPC URL passed to `deposit-prover` (defaults to `--rpc-url`).
        #[arg(long)]
        prover_rpc_url: Option<String>,
        #[arg(long, default_value_t = 18)]
        degree: u32,
        #[arg(long, default_value_t = 2048)]
        max_data_byte_len: usize,
        #[arg(long, default_value_t = 20)]
        max_log_num: usize,
        /// Acki Nacki destination dApp identifier (UInt256), hex. Config tag
        /// bound as the dappId public inputs (not part of the deposit event).
        /// `eccUSDCBridge` pins the deposit's dappId to 0 and ignores this
        /// value, so the default `0` is correct (QC-OFF-09).
        #[arg(long, env = "AN_DAPP_ID", default_value = "0")]
        dapp_id: String,
        /// Where to write `vk_blob.bin` / `public_inputs.bin` / `proof.bin`.
        #[arg(long)]
        out_dir: PathBuf,
        /// Optional expected `eth_chainId` (must be a supported deposit chain).
        #[arg(long)]
        expect_chain_id: Option<u64>,
    },
    /// Long-running listen→prove→submit loop.
    Daemon {
        #[arg(long)]
        rpc_url: String,
        #[arg(long)]
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
        /// Working directory of the `deposit-prover` examples: the proving
        /// key under `data/` and `data/kzg_params_18.srs` are read there.
        #[arg(long, env = "DEPOSIT_PROVER_DIR")]
        deposit_prover_dir: PathBuf,
        /// Prebuilt `fetch_deposit_data` example (`cargo build --release
        /// --examples` in `deposit-prover/`). Checked to exist and be
        /// executable at startup.
        #[arg(long, env = "DEPOSIT_FETCH_DEPOSIT_DATA_BIN")]
        fetch_deposit_data_bin: PathBuf,
        /// Prebuilt `export_vk_blob` example.
        #[arg(long, env = "DEPOSIT_EXPORT_VK_BLOB_BIN")]
        export_vk_blob_bin: PathBuf,
        /// Prebuilt `export_blake2b_proof` example.
        #[arg(long, env = "DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN")]
        export_blake2b_proof_bin: PathBuf,
        #[arg(long)]
        prover_rpc_url: Option<String>,
        #[arg(long, default_value_t = 18)]
        degree: u32,
        #[arg(long, default_value_t = 2048)]
        max_data_byte_len: usize,
        #[arg(long, default_value_t = 20)]
        max_log_num: usize,
        /// Acki Nacki destination dApp identifier (UInt256), hex. Config tag
        /// bound as the dappId public inputs (not part of the deposit event).
        /// `eccUSDCBridge` pins the deposit's dappId to 0 and ignores this
        /// value, so the default `0` is correct (QC-OFF-09).
        #[arg(long, env = "AN_DAPP_ID", default_value = "0")]
        dapp_id: String,
        /// tvm_client 3.0 endpoint. The host must serve GraphQL *and*
        /// `/v2/messages` (`send_message`). A public gateway that only
        /// answers `/graphql` is not enough. Example:
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
        /// Accept a state file stamped for another deployment (chain, bridge
        /// or dappId) by starting it over: progress, scan cursor and parked
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
        #[arg(long)]
        expect_chain_id: Option<u64>,
        /// Bind address of the Prometheus text exporter (`GET /metrics`), e.g.
        /// `127.0.0.1:9467`. Unset = no exporter. Metric names are listed in
        /// `deposit_relayer_daemon::metrics`.
        #[arg(long, env = "DEPOSIT_RELAYER_METRICS_ADDR")]
        metrics_addr: Option<SocketAddr>,
    },
    /// Submit an already-generated proof bundle to `USDCBridge.finalizeDeposit`
    /// on Acki Nacki, bypassing the Ethereum listen/prove stages.
    ///
    /// Reads `public_inputs.bin` + `proof.bin` (the layout `prove-one` writes;
    /// `vk_blob.bin` is optional — the deployed contract uses its embedded
    /// `VK_BLOB`) from `--bundle-dir` and fires the live `(proof,
    /// publicInputs)` call. This is the operator path for finalising a
    /// specific deposit and the contract/node readiness self-test (does
    /// shellnet's `ZKHALO2VERIFYWITHVK` accept a real deposit proof?).
    FinalizeOne {
        /// Directory holding `public_inputs.bin`, `proof.bin` (and optionally
        /// `vk_blob.bin`) — e.g. a `prove-one --out-dir`.
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
        /// Path to the deployed `USDCBridge.abi.json`.
        #[arg(long, env = "AN_BRIDGE_ABI_PATH")]
        an_bridge_abi_path: String,
        /// Bridge contract address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_TOKEN_BRIDGE")]
        an_token_bridge: String,
        /// Relayer signer address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_SENDER")]
        an_sender: String,
    },
    /// Anchor the Arbitrum blocks that hold deposits: follow the parent
    /// chain's batches and assertions and call `proveBlock` on the chain's
    /// `ArbitrumBlockVerifier`. Runs next to `daemon`, which finalizes the
    /// deposits once their blocks are accepted.
    ArbitrumAnchors {
        /// Parent-chain RPC: Sepolia for Arbitrum Sepolia, Ethereum for
        /// Arbitrum One. Must serve `eth_getBlockReceipts`.
        #[arg(long, env = "PARENT_RPC_URL", hide_env_values = true)]
        parent_rpc_url: String,
        /// Arbitrum RPC. No archive node: proofs are taken as each batch
        /// lands, while its state is seconds old.
        #[arg(long, env = "ARBITRUM_RPC_URL", hide_env_values = true)]
        arbitrum_rpc_url: String,
        /// `AckiNackiBridge` on the Arbitrum chain, whose `Deposit`s are
        /// anchored.
        #[arg(long)]
        bridge_address: Address,
        /// First Arbitrum block to scan for deposits (the bridge's deploy
        /// block). When `0`, falls back to `BRIDGE_DEPLOY_BLOCK`.
        #[arg(long, default_value_t = 0)]
        from_block: u64,
        /// tvm_client 3.0 endpoint that serves GraphQL and `/v2/messages`.
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: String,
        /// tvm-cli keys JSON that signs the `proveBlock` messages (any key:
        /// the call is permissionless).
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: String,
        /// `eccUSDCBridge.abi.json` on the AN side.
        #[arg(long, env = "AN_BRIDGE_ABI_PATH")]
        an_bridge_abi_path: String,
        /// `ArbitrumBlockVerifier.abi.json`
        /// (`contracts/an/0.81.0_compiled/exchange/`).
        #[arg(long, env = "AN_ARBITRUM_VERIFIER_ABI_PATH")]
        an_verifier_abi_path: String,
        /// Bridge contract address (`dapp_id::account_id`, SDK 3.0 form).
        #[arg(long, env = "AN_TOKEN_BRIDGE")]
        an_token_bridge: String,
        /// Where the proofs taken at batch time are kept until their
        /// assertion lands. They cannot be fetched again later: keep the file
        /// across restarts.
        #[arg(
            long,
            env = "ARBITRUM_ANCHOR_STATE",
            default_value = "./arbitrum-anchors-state.json"
        )]
        anchor_state: PathBuf,
        #[arg(long, default_value_t = 12)]
        poll_secs: u64,
        /// Blocks per `eth_getLogs` on the parent chain.
        #[arg(long, default_value_t = 1_000)]
        parent_log_span: u64,
        /// Blocks per `eth_getLogs` on the Arbitrum chain.
        #[arg(long, default_value_t = 10_000)]
        arbitrum_log_span: u64,
        /// Allow non-HTTPS GraphQL endpoints (local dev only).
        #[arg(long)]
        allow_insecure_graphql: bool,
    },
    /// Write one `proveBlock` vector for the block `--behind` blocks before
    /// the Arbitrum head, built as `arbitrum-anchors` builds its calls. Waits
    /// for the first assertion that ends at a batch seen after the start:
    /// about one to two assertion intervals. Sends nothing.
    ArbitrumVector {
        #[arg(long, env = "PARENT_RPC_URL", hide_env_values = true)]
        parent_rpc_url: String,
        #[arg(long, env = "ARBITRUM_RPC_URL", hide_env_values = true)]
        arbitrum_rpc_url: String,
        #[arg(long, default_value_t = 1_000)]
        behind: u64,
        #[arg(long, default_value_t = 100)]
        timeout_mins: u64,
        /// The vector, in the format of
        /// `scripts/l2-block-proof-bench/fixtures/`.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 12)]
        poll_secs: u64,
        #[arg(long, default_value_t = 1_000)]
        parent_log_span: u64,
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
        Cmd::ArbitrumAnchors {
            parent_rpc_url,
            arbitrum_rpc_url,
            bridge_address,
            from_block,
            an_graphql_url,
            an_keys_path,
            an_bridge_abi_path,
            an_verifier_abi_path,
            an_token_bridge,
            anchor_state,
            poll_secs,
            parent_log_span,
            arbitrum_log_span,
            allow_insecure_graphql,
        } => {
            let an = AnConfig {
                graphql_url: an_graphql_url,
                keys_path: an_keys_path,
                bridge_abi_path: an_bridge_abi_path,
                token_bridge: an_token_bridge,
                ..AnConfig::default()
            };
            an.validate_live_graphql_endpoint(allow_insecure_graphql)?;
            arbitrum_anchors(
                &parent_rpc_url,
                &arbitrum_rpc_url,
                bridge_address,
                resolve_from_block(from_block),
                an,
                &an_verifier_abi_path,
                &anchor_state,
                Duration::from_secs(poll_secs.max(1)),
                parent_log_span,
                arbitrum_log_span,
            )
            .await
            .map_err(log_err("arbitrum-anchors"))
        },
        Cmd::ArbitrumVector {
            parent_rpc_url,
            arbitrum_rpc_url,
            behind,
            timeout_mins,
            out,
            poll_secs,
            parent_log_span,
        } => arbitrum_vector(
            &parent_rpc_url,
            &arbitrum_rpc_url,
            behind,
            Duration::from_secs(timeout_mins * 60),
            &out,
            Duration::from_secs(poll_secs.max(1)),
            parent_log_span,
        )
        .await
        .map_err(log_err("arbitrum-vector")),
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
            deposit_prover_dir,
            fetch_deposit_data_bin,
            export_vk_blob_bin,
            export_blake2b_proof_bin,
            prover_rpc_url,
            degree,
            max_data_byte_len,
            max_log_num,
            dapp_id,
            out_dir,
            expect_chain_id,
        } => {
            let dapp_id = parse_and_validate_dapp_id(&dapp_id).map_err(|e| anyhow::anyhow!(e))?;
            let prover_cfg = build_prover_cfg(
                deposit_prover_dir,
                DepositProverBins {
                    fetch_deposit_data: fetch_deposit_data_bin,
                    export_vk_blob: export_vk_blob_bin,
                    export_blake2b_proof: export_blake2b_proof_bin,
                },
                prover_rpc_url.unwrap_or_else(|| rpc_url.clone()),
                degree,
                max_data_byte_len,
                max_log_num,
                dapp_id,
            )?;
            prove_one(
                rpc_url,
                bridge_address,
                deposit_id,
                resolve_from_block(from_block),
                confirmations,
                tx_hash,
                log_index,
                prover_cfg,
                out_dir,
                expect_chain_id,
            )
            .await
            .map_err(log_err("prove-one"))
        },
        Cmd::FinalizeOne {
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_bridge_abi_path,
            an_token_bridge,
            an_sender,
        } => finalize_one(
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_bridge_abi_path,
            an_token_bridge,
            an_sender,
        )
        .await
        .map_err(log_err("finalize-one")),
        Cmd::Daemon {
            rpc_url,
            bridge_address,
            from_block,
            confirmations,
            start_deposit_id,
            deposit_prover_dir,
            fetch_deposit_data_bin,
            export_vk_blob_bin,
            export_blake2b_proof_bin,
            prover_rpc_url,
            degree,
            max_data_byte_len,
            max_log_num,
            dapp_id,
            an_graphql_url,
            an_keys_path,
            an_bridge_abi_path,
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
            let dapp_id = parse_and_validate_dapp_id(&dapp_id).map_err(|e| anyhow::anyhow!(e))?;
            info!(%dapp_id, dry_run, "configured AN_DAPP_ID for deposit proofs");
            let prover_cfg = build_prover_cfg(
                deposit_prover_dir,
                DepositProverBins {
                    fetch_deposit_data: fetch_deposit_data_bin,
                    export_vk_blob: export_vk_blob_bin,
                    export_blake2b_proof: export_blake2b_proof_bin,
                },
                prover_rpc_url.unwrap_or_else(|| rpc_url.clone()),
                degree,
                max_data_byte_len,
                max_log_num,
                dapp_id,
            )?;
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
            if let Some(path) = an_bridge_abi_path {
                an_cfg.bridge_abi_path = path;
            }
            if let Some(addr) = an_token_bridge {
                an_cfg.token_bridge = addr;
            }
            if let Some(addr) = an_sender {
                an_cfg.sender = addr;
            }
            run_daemon(
                args.state,
                rpc_url,
                bridge_address,
                resolve_from_block(from_block),
                confirmations,
                start_deposit_id,
                prover_cfg,
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
    an_bridge_abi_path: String,
    an_token_bridge: String,
    an_sender: String,
) -> anyhow::Result<()> {
    // `vk_blob.bin` is optional: the deployed contract verifies against its own
    // embedded `VK_BLOB`, so the bundle's copy is informational only.
    let vk_blob = std::fs::read(bundle_dir.join("vk_blob.bin")).unwrap_or_default();
    let public_inputs = std::fs::read(bundle_dir.join("public_inputs.bin"))
        .map_err(|e| anyhow::anyhow!("read {}/public_inputs.bin: {e}", bundle_dir.display()))?;
    let proof = std::fs::read(bundle_dir.join("proof.bin"))
        .map_err(|e| anyhow::anyhow!("read {}/proof.bin: {e}", bundle_dir.display()))?;

    let bundle = DepositProofBundle::from_operands(vk_blob, public_inputs, proof)?;
    let pi = &bundle.parsed;
    info!(
        deposit_id = %pi.deposit_id,
        sender = %pi.sender,
        amount = %pi.amount,
        contract_address = %pi.contract_address,
        an_account = %pi.an_account(),
        dapp_id = %pi.dapp_id(),
        proof_len = bundle.proof.len(),
        public_inputs_len = bundle.public_inputs.len(),
        "submitting finalizeDeposit(proof, publicInputs) to AN",
    );

    let keys_json = std::fs::read_to_string(&an_keys_path)?;
    let keys: KeyPair = serde_json::from_str(&keys_json)?;
    let bridge_abi = TvmAckiNacki::load_abi(&an_bridge_abi_path)
        .map_err(|e| anyhow::anyhow!("load bridge ABI: {e}"))?;
    let tvm = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi,
    })
    .map_err(|e| anyhow::anyhow!("connect tvm_client: {e}"))?;

    let submit_cfg = AnSubmitConfig {
        from: an_sender,
        token_bridge: an_token_bridge,
        confirm_timeout_secs: 60,
    };
    let submitter = AnInterfaceSubmitter::new(Arc::new(tvm), submit_cfg);

    match submitter.submit_bundle(&bundle).await? {
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
            info!("deposit already finalised on AN (nullifier set)");
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

/// The prover configuration, its binaries and working directory checked
/// before anything connects: a wrong path is a configuration error, found
/// at startup rather than at the first deposit.
#[allow(clippy::too_many_arguments)]
fn build_prover_cfg(
    deposit_prover_dir: PathBuf,
    bins: DepositProverBins,
    rpc_url: String,
    degree: u32,
    max_data_byte_len: usize,
    max_log_num: usize,
    dapp_id: String,
) -> anyhow::Result<SubprocessProverConfig> {
    anyhow::ensure!(
        deposit_prover_dir.is_dir(),
        "--deposit-prover-dir {} is not a directory",
        deposit_prover_dir.display()
    );
    let mut cfg = SubprocessProverConfig::new(deposit_prover_dir, bins.validated()?, rpc_url);
    cfg.degree = degree;
    cfg.max_data_byte_len = max_data_byte_len;
    cfg.max_log_num = max_log_num;
    cfg.dapp_id = dapp_id;
    Ok(cfg)
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
    prover_cfg: SubprocessProverConfig,
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
    let _chain_id = ensure_supported_rpc_chain(&provider, expect_chain_id).await?;

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
        let source = EthLogSource::new(provider, bridge_address, from_block, confirmations);
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
    force_state: bool,
    skip_after_attempts: u32,
    outstanding_backoff_secs: u64,
    anchor_wait_warn_after_secs: u64,
    allow_insecure_graphql: bool,
    expect_chain_id: Option<u64>,
) -> anyhow::Result<()> {
    let _state_lock = StateLock::acquire(&state_path)
        .map_err(|e| anyhow::anyhow!("failed to acquire state lock: {e}"))?;

    let existing_state = RelayerState::load(&state_path)?.unwrap_or_default();
    let scan_cursor = Arc::new(Mutex::new(existing_state.scan_done_through_block));

    // Same gate as `watch` / `prove-one`: an unsupported chain produces proofs
    // the AN-side bridge has no allowlist entry for, so fail before the first
    // deposit rather than after a long prove.
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = ensure_supported_rpc_chain(&provider, expect_chain_id).await?;
    info!(
        chain_id,
        chain = deposit_relayer_daemon::supported_deposit_chain_name(chain_id).unwrap_or("?"),
        "deposit source chain",
    );
    let deployment = DeploymentIdentity::new(chain_id, bridge_address, prover_cfg.dapp_id.clone());

    let source = Arc::new(
        EthLogSource::new(provider, bridge_address, from_block, confirmations)
            .with_scan_cursor(scan_cursor.clone()),
    );
    deposit_relayer_daemon::metrics::set_build_info();
    spawn_deposit_counter_poller(source.clone());
    let prover = Arc::new(SubprocessProofGenerator::new(prover_cfg));

    let skip_after = if skip_after_attempts == 0 {
        None
    } else {
        Some(skip_after_attempts)
    };

    if dry_run {
        warn!(
            "running in --dry-run mode: deposits are proven against the real chain but \
             'finalized' only in an in-memory mock AN"
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
        let bridge_abi = TvmAckiNacki::load_abi(&an_cfg.bridge_abi_path)
            .map_err(|e| anyhow::anyhow!("load bridge ABI: {e}"))?;
        let tvm = TvmAckiNacki::connect(TvmClientConfig {
            graphql_endpoints: vec![an_cfg.graphql_url.clone()],
            keys,
            bridge_abi,
        })
        .map_err(|e| anyhow::anyhow!("connect tvm_client: {e}"))?;
        let submit_cfg = an_cfg.to_submit_config();
        let voucher_abi = TvmAckiNacki::load_abi(&an_cfg.bridge_abi_path)
            .map_err(|e| anyhow::anyhow!("load bridge ABI: {e}"))?;
        let submitter =
            AnInterfaceSubmitter::new(Arc::new(tvm), submit_cfg).with_voucher_check(voucher_abi)?;
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
            prover,
            Arc::new(submitter),
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_loop<S, P, A>(
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
        deployment: Some(deployment),
        force_state,
        skip_after_attempts,
        outstanding_backoff_secs,
        anchor_wait_warn_after_secs,
        scan_cursor: Some(scan_cursor),
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

/// The Arbitrum chain behind `arbitrum_rpc_url`, checked against the parent
/// chain behind `parent_rpc_url`.
async fn arbitrum_view(
    parent_rpc_url: &str,
    arbitrum_rpc_url: &str,
    evm_bridge: Address,
    parent_log_span: u64,
    arbitrum_log_span: u64,
) -> anyhow::Result<arbitrum::RpcArbitrumView<impl Provider, impl Provider>> {
    use alloy::providers::Provider as _;
    let parent = ProviderBuilder::new().connect_http(parent_rpc_url.parse()?);
    let arb = ProviderBuilder::new().connect_http(arbitrum_rpc_url.parse()?);
    let id = arb.get_chain_id().await?;
    let chain = *arbitrum::arbitrum_chain(id).ok_or_else(|| {
        anyhow::anyhow!(
            "chain {id} behind --arbitrum-rpc-url is not an Arbitrum chain the bridge proves"
        )
    })?;
    let parent_id = parent.get_chain_id().await?;
    anyhow::ensure!(
        parent_id == chain.parent_chain_id,
        "--parent-rpc-url is chain {parent_id}; {} posts to {}",
        chain.name,
        chain.parent_chain_id
    );
    // `proveBlock`'s receipt proof is built from a whole block's receipts:
    // an RPC without the call would pin the parent cursor on every assertion.
    let head = parent.get_block_number().await?;
    parent.get_block_receipts(head.into()).await.map_err(|e| {
        anyhow::anyhow!("--parent-rpc-url does not serve eth_getBlockReceipts: {e}")
    })?;
    Ok(arbitrum::RpcArbitrumView {
        parent,
        arbitrum: arb,
        chain,
        evm_bridge,
        parent_log_span,
        arbitrum_log_span,
        batch_txs: Default::default(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn arbitrum_anchors(
    parent_rpc_url: &str,
    arbitrum_rpc_url: &str,
    evm_bridge: Address,
    from_block: u64,
    an: AnConfig,
    verifier_abi_path: &str,
    state_path: &std::path::Path,
    poll: Duration,
    parent_log_span: u64,
    arbitrum_log_span: u64,
) -> anyhow::Result<()> {
    use alloy::providers::Provider as _;
    let view = arbitrum_view(
        parent_rpc_url,
        arbitrum_rpc_url,
        evm_bridge,
        parent_log_span,
        arbitrum_log_span,
    )
    .await?;
    let chain = view.chain;
    let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an.keys_path)?)?;
    let target = arbitrum::TvmAnchorTarget::connect(
        &an.graphql_url,
        keys,
        &an.bridge_abi_path,
        verifier_abi_path,
        an.token_bridge_address()?,
        &chain,
    )
    .await?;
    let mut state = match arbitrum::AnchorState::load(state_path)? {
        Some(s) => {
            anyhow::ensure!(
                s.chain_id == chain.chain_id,
                "{} is the state of chain {}, not {} ({})",
                state_path.display(),
                s.chain_id,
                chain.name,
                chain.chain_id
            );
            let mut s = s;
            s.ensure_bridge(evm_bridge)
                .map_err(|e| anyhow::anyhow!("{}: {e}", state_path.display()))?;
            s
        },
        // Rolls forward from now: deposits from --from-block on are anchored
        // by the next batch after the start.
        None if from_block == 0 => anyhow::bail!(
            "arbitrum-anchors has no state at {} and no --from-block: pass the bridge's deploy \
             block (or set {}), or it scans {} from block 0",
            state_path.display(),
            BRIDGE_DEPLOY_BLOCK_ENV,
            chain.name
        ),
        None => arbitrum::AnchorState {
            chain_id: chain.chain_id,
            evm_bridge: Some(evm_bridge),
            parent_cursor: view.parent.get_block_number().await?,
            deposit_cursor: from_block.saturating_sub(1),
            ..Default::default()
        },
    };
    *view.batch_txs.lock().unwrap_or_else(|p| p.into_inner()) = state.batch_txs.clone();
    info!(
        chain = chain.name,
        parent_cursor = state.parent_cursor,
        deposit_cursor = state.deposit_cursor,
        pending = state.pending.len(),
        ready = state.ready.len(),
        "arbitrum-anchors starting"
    );
    let cfg = arbitrum::TickConfig::default();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        match arbitrum::tick(&chain, &view, &target, &mut state, &cfg).await {
            Ok(r) if r != arbitrum::TickReport::default() => info!(?r, "arbitrum-anchors tick"),
            Ok(_) => {},
            Err(e) => warn!(error = %e, "arbitrum-anchors tick failed; retrying"),
        }
        state.batch_txs = view
            .batch_txs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        state.save(state_path)?;
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(poll) => {},
        }
    }
    info!("arbitrum-anchors stopped");
    Ok(())
}

async fn arbitrum_vector(
    parent_rpc_url: &str,
    arbitrum_rpc_url: &str,
    behind: u64,
    timeout: Duration,
    out: &std::path::Path,
    poll: Duration,
    parent_log_span: u64,
) -> anyhow::Result<()> {
    use alloy::providers::Provider as _;
    let view = arbitrum_view(
        parent_rpc_url,
        arbitrum_rpc_url,
        Address::ZERO,
        parent_log_span,
        1,
    )
    .await?;
    let chain = view.chain;
    let mut state = arbitrum::AnchorState {
        chain_id: chain.chain_id,
        parent_cursor: view.parent.get_block_number().await?,
        deposit_cursor: 0,
        ..Default::default()
    };
    let view = arbitrum::OneBlockBehind {
        inner: view,
        behind,
        taken: Default::default(),
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let cfg = arbitrum::TickConfig::default();
    info!(
        chain = chain.name,
        behind, "waiting for an assertion that ends at a batch seen from now on"
    );
    while tokio::time::Instant::now() < deadline {
        match arbitrum::tick(&chain, &view, &arbitrum::NoBridge, &mut state, &cfg).await {
            Ok(r) if r != arbitrum::TickReport::default() => info!(?r, "arbitrum-vector tick"),
            Ok(_) => {},
            Err(e) => warn!(error = %e, "tick failed; retrying"),
        }
        if let Some(proof) = state.ready.first() {
            let (&n, _) = proof
                .end
                .slots
                .iter()
                .next()
                .expect("a ready proof carries a slot");
            let vector = arbitrum::vector_json(&chain, proof, n, state.pending[&n]);
            std::fs::write(out, serde_json::to_string_pretty(&vector)? + "\n")?;
            info!(out = %out.display(), m = proof.end.number, n, "vector written");
            return Ok(());
        }
        tokio::time::sleep(poll).await;
    }
    anyhow::bail!(
        "no assertion ended at a batch seen within {} min",
        timeout.as_secs() / 60
    )
}

/// SIGINT, or SIGTERM on unix.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("SIGINT received"),
                _ = sigterm.recv() => info!("SIGTERM received"),
            },
            Err(e) => {
                warn!(?e, "failed to install SIGTERM handler; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
            },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("SIGINT received");
    }
}

/// Serve the Prometheus text format at `http://<addr>/metrics` for the
/// lifetime of the process. Every `metrics::*` macro in this binary and in
/// `deposit_relayer_daemon` records into it. Same buckets as the AN→ETH
/// relayer: sub-second RPC round-trips up to ten-minute proving stages.
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
/// independent of the tick loop, so the backlog is visible while a proof runs.
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

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}
