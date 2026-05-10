//! Phase 5.1 — Relayer CLI binary.
//!
//! Demonstrates the wiring of [`bridge_relayer_daemon::Relayer`] against:
//! - a [`bridge_relayer_daemon::FixturesBlockSource`] (canned single-block
//!   smoke test from the Phase 4.1 bound proof artefacts);
//! - the production [`bridge_relayer_daemon::EthBridgeClient`] over a
//!   user-supplied `--rpc-url` and `--bridge-address`.
//!
//! In Phase 5.2 this binary grows a `LiveBlockSource` impl. For now,
//! `cargo run -p bridge-relayer-daemon --bin relayer -- --help` is the
//! best entry point.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use ethers::prelude::*;
use ethers::signers::{LocalWallet, Signer};
use tracing::{error, info};

use bridge_relayer_daemon::{
    EthBridgeClient, FixturesBlockSource, Relayer, RelayerConfig, TickOutcome,
};

#[derive(Parser, Debug)]
#[command(name = "relayer", about = "Acki Nacki → Ethereum bridge relayer (Phase 5.1 skeleton)")]
struct Args {
    /// Where to persist `state.json`.
    #[arg(long, default_value = "./relayer-state.json")]
    state: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Submit one canned block from a Phase 4.1 fixture directory.
    SmokeFixture {
        /// Directory containing `bound_scenario.json` +
        /// `primary/groth16_output.json` + `layer-hashes/groth16_output.json`.
        #[arg(long)]
        fixtures_dir: PathBuf,
        /// Ethereum RPC URL (HTTP).
        #[arg(long)]
        rpc_url: String,
        /// `AckiNackiBridge` contract address.
        #[arg(long)]
        bridge_address: Address,
        /// Hex-encoded private key of the relayer EOA.
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        /// Maximum ticks to run before exiting (1 is enough for a
        /// canned fixture).
        #[arg(long, default_value_t = 1)]
        max_ticks: usize,
    },
    /// Print parsed config and exit (for `--help`-style smoke checks).
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = Args::parse();

    match args.cmd {
        Cmd::Status => {
            info!("relayer state file = {}", args.state.display());
            Ok(())
        }
        Cmd::SmokeFixture {
            fixtures_dir,
            rpc_url,
            bridge_address,
            private_key,
            max_ticks,
        } => smoke_fixture(args.state, fixtures_dir, rpc_url, bridge_address, private_key, max_ticks)
            .await
            .map_err(|e| {
                error!(?e, "smoke run failed");
                e
            }),
    }
}

async fn smoke_fixture(
    state_path: PathBuf,
    fixtures_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    max_ticks: usize,
) -> anyhow::Result<()> {
    let provider = Provider::<Http>::try_from(rpc_url.as_str())?;
    let chain_id = provider.get_chainid().await?.as_u64();
    let wallet: LocalWallet = private_key.parse::<LocalWallet>()?.with_chain_id(chain_id);
    let client = Arc::new(SignerMiddleware::new(provider, wallet));

    let bridge = Arc::new(EthBridgeClient::new(bridge_address, client));
    let source = Arc::new(FixturesBlockSource::from_dir(&fixtures_dir)?);

    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, bridge)?;

    let history = relayer
        .run_loop(max_ticks, |outcome| matches!(outcome, TickOutcome::Verified { .. }))
        .await?;
    info!(?history, "smoke run complete");
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}
