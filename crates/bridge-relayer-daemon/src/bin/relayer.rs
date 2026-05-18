//! Phase 5.1 — Relayer CLI binary.
//!
//! Two subcommands:
//!
//! - `smoke-fixture` — submits one canned block from a Phase 4.1 bound-proof
//!   fixture directory through the real
//!   [`bridge_relayer_daemon::EthBridgeClient`]. Optionally wraps the relayer
//!   in a [`bridge_relayer_daemon::SentryGuardedRelayer`] when `--an-node-url`
//!   is provided, so a live AN-side BK rotation pauses the run end-to-end.
//!
//! - `sentry-watch` — a standalone BK-set sentry: polls `/v2/bk_set_update` on
//!   the supplied AN node and prints structured `Bootstrapped` / `Quiet` /
//!   `RotationDetected` events. No Ethereum side is touched — handy for
//!   operators wanting to confirm that the committee they think is active
//!   really is, before running the bridge relayer in earnest.
//!
//! In Phase 5.2 the binary gains a `LiveBlockSource` impl and the
//! sentry's `resume()` gets wired into the rotation pipeline. For now,
//! `cargo run -p bridge-relayer-daemon --bin relayer -- --help` is the
//! best entry point.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    signers::{local::PrivateKeySigner, Signer},
};
use bridge_relayer_daemon::{
    BkSetSentry, EthBridgeClient, FixturesBlockSource, GuardedOutcome, Relayer, RelayerConfig,
    SentryGuardedRelayer, SentryStatus, TickOutcome,
};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "relayer",
    about = "Acki Nacki → Ethereum bridge relayer (Phase 5.1 skeleton)"
)]
struct Args {
    /// Where to persist `state.json`. Only used by the `smoke-fixture`
    /// subcommand; `sentry-watch` is stateless.
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
        /// Optional AN node base URL (e.g. `http://94.156.178.19:8600`).
        /// When supplied, the relayer is wrapped in a
        /// `SentryGuardedRelayer` that pauses verifyBlock submissions
        /// on a live BK rotation. Without it the relayer runs blind
        /// (Phase 5.1 behaviour, suitable only for canned fixtures).
        #[arg(long, env = "AN_NODE_URL")]
        an_node_url: Option<String>,
    },
    /// Standalone BK-set sentry: poll `/v2/bk_set_update` on an AN
    /// node and print structured events. No Ethereum side.
    SentryWatch {
        /// AN node base URL (e.g. `http://94.156.178.19:8600`).
        #[arg(long, default_value = "http://94.156.178.19:8600")]
        node_url: String,
        /// Number of ticks before exiting. `0` means run forever
        /// (Ctrl-C to stop).
        #[arg(long, default_value_t = 5)]
        ticks: u64,
        /// Seconds between ticks.
        #[arg(long, default_value_t = 30)]
        interval_secs: u64,
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
        },
        Cmd::SmokeFixture {
            fixtures_dir,
            rpc_url,
            bridge_address,
            private_key,
            max_ticks,
            an_node_url,
        } => smoke_fixture(
            args.state,
            fixtures_dir,
            rpc_url,
            bridge_address,
            private_key,
            max_ticks,
            an_node_url,
        )
        .await
        .map_err(|e| {
            error!(?e, "smoke run failed");
            e
        }),
        Cmd::SentryWatch {
            node_url,
            ticks,
            interval_secs,
        } => sentry_watch(node_url, ticks, interval_secs)
            .await
            .map_err(|e| {
                error!(?e, "sentry watch failed");
                e
            }),
    }
}

#[allow(clippy::too_many_arguments)] // CLI surface; each arg maps to a flag
async fn smoke_fixture(
    state_path: PathBuf,
    fixtures_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    max_ticks: usize,
    an_node_url: Option<String>,
) -> anyhow::Result<()> {
    // alloy migration (2026-05-17): `Provider<Http>::try_from(url)` +
    // `LocalWallet` + `SignerMiddleware` is replaced by a builder-style
    // chain that yields a wallet-filled provider in a single call. The
    // signer's chain ID is derived from the RPC endpoint to mirror the
    // ethers behaviour (which called `get_chainid` before `with_chain_id`).
    let signer: PrivateKeySigner = private_key.parse()?;
    let probe_provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe_provider.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);

    let bridge = Arc::new(EthBridgeClient::new(bridge_address, provider));
    let source = Arc::new(FixturesBlockSource::from_dir(&fixtures_dir)?);

    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, bridge)?;

    match an_node_url {
        None => {
            info!("running without sentry (BK rotations will NOT pause this relayer)");
            let history = relayer
                .run_loop(max_ticks, |outcome| {
                    matches!(outcome, TickOutcome::Verified { .. })
                })
                .await?;
            info!(?history, "smoke run complete");
        },
        Some(url) => {
            info!(node_url = %url, "running with BkSetSentry guard");
            let sentry = BkSetSentry::from_node_url(url)?;
            let mut guarded = SentryGuardedRelayer::new(relayer, sentry);
            for tick_idx in 1..=max_ticks {
                let outcome = guarded.tick().await?;
                match &outcome {
                    GuardedOutcome::SentryBootstrapped {
                        observed_seq_no,
                        bk_count,
                        inner,
                    } => {
                        info!(
                            tick = tick_idx,
                            observed_seq_no,
                            bk_count,
                            ?inner,
                            "bootstrap + inner",
                        );
                        if matches!(inner, TickOutcome::Verified { .. }) {
                            return Ok(());
                        }
                    },
                    GuardedOutcome::SentryQuiet {
                        observed_seq_no,
                        future_changed,
                        inner,
                    } => {
                        info!(
                            tick = tick_idx,
                            observed_seq_no,
                            future_changed,
                            ?inner,
                            "quiet + inner",
                        );
                        if matches!(inner, TickOutcome::Verified { .. }) {
                            return Ok(());
                        }
                    },
                    GuardedOutcome::RotationDetected {
                        old_seq_no,
                        new_seq_no,
                        added,
                        removed,
                        pubkey_mutations,
                    } => {
                        warn!(
                            tick = tick_idx,
                            old_seq_no,
                            new_seq_no,
                            added,
                            removed,
                            pubkey_mutations,
                            "BK rotation detected — relayer paused; Phase 5.2 will trigger \
                             Circuit 3 and call resume() here. Exiting the smoke run.",
                        );
                        return Ok(());
                    },
                    GuardedOutcome::PausedAwaitingRotationReconcile => {
                        // Unreachable in the smoke binary (we exit on
                        // the first RotationDetected above) — kept
                        // for exhaustiveness.
                        warn!(tick = tick_idx, "guard still paused; exiting");
                        return Ok(());
                    },
                }
            }
            info!(
                "smoke run complete (no verified block within {} ticks)",
                max_ticks
            );
        },
    }
    Ok(())
}

async fn sentry_watch(node_url: String, ticks: u64, interval_secs: u64) -> anyhow::Result<()> {
    let mut sentry = BkSetSentry::from_node_url(&node_url)?;
    info!(%node_url, ticks, interval_secs, "starting BkSetSentry");

    let interval = Duration::from_secs(interval_secs);
    let cap = if ticks == 0 { u64::MAX } else { ticks };
    for i in 1..=cap {
        match sentry.tick().await {
            Ok(SentryStatus::Bootstrapped {
                observed_seq_no,
                bk_count,
            }) => {
                info!(
                    tick = i,
                    observed_seq_no, bk_count, "BOOTSTRAP — first observation"
                );
            },
            Ok(SentryStatus::Quiet {
                observed_seq_no,
                future_changed,
            }) => {
                info!(
                    tick = i,
                    observed_seq_no, future_changed, "QUIET — membership unchanged"
                );
            },
            Ok(SentryStatus::RotationDetected {
                old_seq_no,
                new_seq_no,
                delta,
            }) => {
                warn!(
                    tick = i,
                    old_seq_no,
                    new_seq_no,
                    added = delta.added.len(),
                    removed = delta.removed.len(),
                    pubkey_mutations = delta.pubkey_mutations.len(),
                    "ROTATION — committee changed",
                );
            },
            Err(e) => {
                warn!(tick = i, error = ?e, "sentry tick failed; continuing");
            },
        }
        let metrics = sentry.metrics();
        info!(
            tick = i,
            total = metrics.total_ticks,
            ok = metrics.successful_ticks,
            rotations = metrics.rotations_observed,
            last_seq_no = metrics.last_observed_seq_no,
            "metrics snapshot",
        );
        if i < cap {
            tokio::time::sleep(interval).await;
        }
    }
    let metrics = sentry.metrics();
    info!(?metrics, "sentry watch complete");
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
