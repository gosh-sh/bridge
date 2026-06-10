//! Phase 5.1 — Relayer CLI binary.
//!
//! Four subcommands:
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
//! - `daemon` — long-running operator entry point. Drives `Relayer::tick`
//!   forever with exponential backoff, until SIGINT/SIGTERM. Optionally wraps
//!   in `SentryGuardedRelayer` when `--an-node-url` is supplied so the loop
//!   pauses on a live BK rotation. Logs a structured metrics snapshot on every
//!   shutdown.
//!
//! - `verify-fixture` — **read-only** pre-flight check. Loads a fixture, reads
//!   the on-chain bridge anchors over RPC, and reports field-by-field whether
//!   the fixture would be accepted by `verifyBlock` (the cheap pre-crypto
//!   checks: bk-set commitment match, monotonic seqNo, prev- anchor match). No
//!   private key, no submission. Exits non-zero on any mismatch so it slots
//!   into a pre-deploy shell pipeline.
//!
//! In Phase 5.2 the `daemon` subcommand will swap `FixturesBlockSource`
//! for a real `LiveBlockSource` and the sentry's `resume()` gets wired
//! into the rotation pipeline. For now,
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
    BackoffConfig, BkSetSentry, BlockSource, BridgeClient, DryRunOutcome, EthBridgeClient,
    FixturesBlockSource, GuardedOutcome, PartnerWithdrawalProof, ProverProofsBlockSource, Relayer,
    RelayerConfig, RelayerMetrics, SentryGuardedRelayer, SentryStatus, TickOutcome,
    WithdrawSubmitOutcome,
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
    /// Long-running daemon mode. Drives `Relayer::tick` forever with
    /// exponential backoff until SIGINT/SIGTERM. The fixture source is
    /// kept here as a Phase 5.1 placeholder; Phase 5.2 will swap it for
    /// a `LiveBlockSource` over the partner's GraphQL + BOC pipeline.
    Daemon {
        /// Phase 5.1 placeholder source: one canned block from a bound-
        /// proof fixture directory. The daemon will submit it (once) and
        /// then idle on `NotYetAvailable` with exponential backoff. The
        /// purpose is to exercise the long-running scaffolding under
        /// `cargo run` against a local Anvil; production deployments
        /// will pass a `--source live` flag in Phase 5.2.
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
        /// Optional AN node base URL. When provided, the daemon runs
        /// inside a `SentryGuardedRelayer` and pauses on a live BK
        /// rotation (waiting for the future Phase 5.2 reconcile path).
        #[arg(long, env = "AN_NODE_URL")]
        an_node_url: Option<String>,
        /// Initial backoff sleep on the first non-success outcome.
        #[arg(long, default_value_t = 2)]
        backoff_initial_secs: u64,
        /// Maximum sleep length the backoff is allowed to grow to.
        #[arg(long, default_value_t = 60)]
        backoff_max_secs: u64,
        /// Backoff multiplier (next = min(current × multiplier, max)).
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
    },
    /// Read-only pre-flight check. Loads a fixture, reads the on-chain
    /// bridge anchors over RPC, then (by default) `eth_call`-simulates
    /// the full `verifyBlock(...)` so bad proofs surface here too. No
    /// private key, no transaction, no gas. Exits with code 1 on any
    /// mismatch or simulated revert; suitable for pre-deploy CI.
    ///
    /// What this catches:
    /// - wrong network → bridge contract not deployed at the address
    /// - wrong fixture → bk_set_commitment / prev_anchor mismatches
    /// - stale fixture → seqNo ≤ storedLastSeenBlockSeqNo
    /// - bad ZK proof → eth_call surfaces the Groth16 verifier revert (Solidity
    ///   `AttestationProofRejected` / `LayerHashesProofRejected`)
    /// - disabled bridge → eth_call surfaces `VerifyBlockDisabled`
    ///
    /// Pass `--no-simulate` to skip the eth_call (faster, doesn't run the
    /// ~287 k-gas Groth16 verifier triple inside the call). Useful when
    /// the operator only wants to confirm anchor alignment and trusts the
    /// proof generation pipeline.
    VerifyFixture {
        /// Directory containing `bound_scenario.json` +
        /// `primary/groth16_output.json` + `layer-hashes/groth16_output.json`.
        #[arg(long)]
        fixtures_dir: PathBuf,
        /// Ethereum RPC URL (HTTP). Read-only — no signer needed.
        #[arg(long)]
        rpc_url: String,
        /// `AckiNackiBridge` contract address.
        #[arg(long)]
        bridge_address: Address,
        /// Skip the `eth_call`-based `verifyBlock` simulation. By default
        /// the pre-flight runs the full simulation (catches bad proofs).
        /// `--no-simulate` reduces it to the cheap anchor checks only.
        #[arg(long)]
        no_simulate: bool,
    },
    /// Long-running daemon reading partner `proof_<seqno>.json` bundles
    /// from `bridge-prover-daemon` and submitting `verifyBlock` on Ethereum.
    DaemonProver {
        /// Directory containing `proof_*.json` + `result_*.json` (partner
        /// prover daemon `proofs/` folder).
        #[arg(long, env = "PROVER_PROOFS_DIR")]
        proofs_dir: PathBuf,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        #[arg(long, env = "AN_NODE_URL")]
        an_node_url: Option<String>,
        #[arg(long, default_value_t = 2)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 60)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
        /// Skip the partner `result_*.json` verification gate.
        #[arg(long)]
        skip_verified_gate: bool,
    },
    /// Submit one `verifyBlock` for a specific partner proof bundle.
    SubmitVerifyBlock {
        #[arg(long)]
        proofs_dir: PathBuf,
        #[arg(long)]
        block_seq_no: u64,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        skip_verified_gate: bool,
    },
    /// Read-only pre-flight for a partner `proof_<seqno>.json` bundle.
    VerifyProverProof {
        #[arg(long)]
        proofs_dir: PathBuf,
        #[arg(long)]
        block_seq_no: u64,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long)]
        no_simulate: bool,
        #[arg(long)]
        skip_verified_gate: bool,
        /// Allow ~8 KB Halo2 proof bytes through load (anchor checks only).
        #[arg(long)]
        accept_halo2_proofs: bool,
    },
    /// Submit one Circuit 4 `withdrawByProof` from `proof_event_*.json`.
    SubmitWithdraw {
        #[arg(long)]
        proof_event: PathBuf,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        #[arg(long)]
        dry_run: bool,
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
        Cmd::VerifyFixture {
            fixtures_dir,
            rpc_url,
            bridge_address,
            no_simulate,
        } => verify_fixture(fixtures_dir, rpc_url, bridge_address, !no_simulate)
            .await
            .map_err(|e| {
                error!(?e, "verify-fixture failed");
                e
            }),
        Cmd::Daemon {
            fixtures_dir,
            rpc_url,
            bridge_address,
            private_key,
            an_node_url,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            run_daemon(
                args.state,
                fixtures_dir,
                rpc_url,
                bridge_address,
                private_key,
                an_node_url,
                backoff,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon failed");
                e
            })
        },
        Cmd::DaemonProver {
            proofs_dir,
            rpc_url,
            bridge_address,
            private_key,
            an_node_url,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            skip_verified_gate,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            run_prover_daemon(
                args.state,
                proofs_dir,
                rpc_url,
                bridge_address,
                private_key,
                an_node_url,
                backoff,
                skip_verified_gate,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon-prover failed");
                e
            })
        },
        Cmd::SubmitVerifyBlock {
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
            skip_verified_gate,
        } => submit_verify_block(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
            skip_verified_gate,
        )
        .await
        .map_err(|e| {
            error!(?e, "submit-verify-block failed");
            e
        }),
        Cmd::VerifyProverProof {
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            no_simulate,
            skip_verified_gate,
            accept_halo2_proofs,
        } => verify_prover_proof(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            !no_simulate,
            skip_verified_gate,
            accept_halo2_proofs,
        )
        .await
        .map_err(|e| {
            error!(?e, "verify-prover-proof failed");
            e
        }),
        Cmd::SubmitWithdraw {
            proof_event,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
        } => submit_withdraw(proof_event, rpc_url, bridge_address, private_key, dry_run)
            .await
            .map_err(|e| {
                error!(?e, "submit-withdraw failed");
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

#[allow(clippy::too_many_arguments)] // CLI surface; each arg maps to a flag
async fn run_daemon(
    state_path: PathBuf,
    fixtures_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    an_node_url: Option<String>,
    backoff: BackoffConfig,
) -> anyhow::Result<()> {
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
    let metrics = RelayerMetrics::new();

    // Cross-platform graceful shutdown: SIGINT on every OS, plus SIGTERM
    // on Unix (Docker / systemd send SIGTERM by default).
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

    info!(?backoff, ?an_node_url, "daemon starting");
    let summary = match an_node_url {
        None => {
            warn!(
                "running without sentry — a live BK rotation will NOT pause this daemon; \
                 verifyBlock calls will start reverting until the operator restarts."
            );
            relayer
                .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
                .await?
        },
        Some(url) => {
            info!(node_url = %url, "wrapping in SentryGuardedRelayer");
            let sentry = BkSetSentry::from_node_url(url)?;
            let mut guarded = SentryGuardedRelayer::new(relayer, sentry);
            guarded
                .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
                .await?
        },
    };

    info!(?summary, snapshot = ?metrics.snapshot(), "daemon stopped");
    Ok(())
}

/// Read-only pre-flight check. Connects to the bridge over HTTP, reads
/// the on-chain anchors, and reports field-by-field whether the fixture's
/// payload would pass the *cheap* (pre-crypto) checks in `verifyBlock`.
///
/// Exit code:
/// - `0` on full match (proofs not eth_call'd, but state checks pass);
/// - `1` on any mismatch (with a per-field diagnostic in the log).
///
/// Why no eth_call'ing the verifier: the gnark verifiers cost ~287k gas
/// each and require a live Ethereum node that can simulate the full
/// `verifyBlock` flow including external contract calls. Operators
/// running this in pre-deploy CI usually point at a free RPC where
/// such simulation isn't reliable, and the cheap checks already catch
/// 95 % of operator-side mistakes (wrong network, wrong fixture, stale
/// fixture). The remaining 5 % (bad proofs) only manifests on the real
/// `daemon` submit and is logged as a `Reverted` outcome.
async fn verify_fixture(
    fixtures_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    simulate: bool,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);
    let source = FixturesBlockSource::from_dir(&fixtures_dir)?;

    let on_chain = bridge.read_state().await?;
    // FixturesBlockSource holds exactly one block (seqNo = scenario's
    // seqNo). Trying both `last_seen + 1` and the fixture's own seqNo
    // is overkill — we just probe `last_seen + 1` and fall back to
    // probing the fixture directly so the operator gets a meaningful
    // diagnostic even when the fixture is stale (i.e. `block_seq_no
    // <= last_seen`).
    let probe_target = on_chain.last_seen_block_seq_no.saturating_add(1);
    let block = match source.fetch(probe_target).await? {
        Some(b) => b,
        None => {
            // Fixture doesn't serve `last_seen + 1` — it's either stale
            // (already submitted) or for the wrong bridge. Fetch the
            // fixture's own block via a brute-force walk so we can give
            // a precise diagnostic. The fixtures source only holds one
            // block so this is bounded.
            //
            // We accept any seqNo in 1..=u32::MAX (FixturesBlockSource
            // serialises block_seq_no as u32 today; the real bridge is
            // u64 but the fixture format matches the orchestrator).
            let mut found = None;
            for candidate_seq in 1..=u32::MAX as u64 {
                if let Some(b) = source.fetch(candidate_seq).await? {
                    found = Some(b);
                    break;
                }
            }
            match found {
                Some(b) => b,
                None => {
                    error!("FixturesBlockSource exposed no blocks; the fixture directory is empty");
                    std::process::exit(1);
                },
            }
        },
    };

    info!(
        bridge_address = %bridge_address,
        last_seen = on_chain.last_seen_block_seq_no,
        "on-chain state read",
    );
    info!(
        fixture_seq_no = block.block_seq_no,
        fixture_block_id = ?block.block_id,
        fixture_fin_type = ?block.fin_type,
        fixture_num_layers = block.num_layers,
        "fixture loaded",
    );

    let mut ok = true;
    let mut diagnostics: Vec<String> = Vec::new();

    if block.bk_set_commitment != on_chain.bk_set_commitment {
        ok = false;
        diagnostics.push(format!(
            "BkSetCommitment MISMATCH: fixture = {:#x}, on-chain = {:#x}",
            block.bk_set_commitment, on_chain.bk_set_commitment
        ));
    } else {
        info!(
            bk_set_commitment = ?block.bk_set_commitment,
            "BkSetCommitment matches on-chain",
        );
    }

    if block.block_seq_no <= on_chain.last_seen_block_seq_no {
        ok = false;
        diagnostics.push(format!(
            "BlockSeqNo NOT MONOTONIC: fixture seqNo = {}, on-chain last_seen = {}",
            block.block_seq_no, on_chain.last_seen_block_seq_no
        ));
    } else {
        info!(
            fixture_seq_no = block.block_seq_no,
            on_chain_last_seen = on_chain.last_seen_block_seq_no,
            "seqNo is strictly greater than last_seen",
        );
    }

    if block.prev_max_level_layer_hash != on_chain.prev_max_level_layer_hash {
        ok = false;
        diagnostics.push(format!(
            "PrevAnchor MISMATCH: fixture = {:#x}, on-chain = {:#x}",
            block.prev_max_level_layer_hash, on_chain.prev_max_level_layer_hash
        ));
    } else {
        info!(
            prev_anchor = ?block.prev_max_level_layer_hash,
            "PrevAnchor matches on-chain",
        );
    }

    if !ok {
        for d in &diagnostics {
            error!("{}", d);
        }
        // `process::exit(1)` is the standard CLI signal to a shell
        // pipeline that the check failed. We don't return Err because
        // anyhow then prints the error as "verify-fixture failed:
        // ..." which duplicates the per-field log lines.
        std::process::exit(1);
    }

    info!("verify-fixture: cheap anchor checks PASS");

    if !simulate {
        info!("verify-fixture: --no-simulate set; skipping eth_call verifier triple simulation");
        return Ok(());
    }

    info!(
        "verify-fixture: running eth_call simulation of verifyBlock(...) — this exercises the \
         full Groth16 verifier triple inside the contract; ~1 s on a free RPC"
    );
    match bridge.dry_run_block(&block).await? {
        DryRunOutcome::WouldSucceed => {
            info!(
                "verify-fixture: eth_call simulation PASS; a real submit at the current head \
                 would verify (subject to no on-chain state change between now and submit)"
            );
            Ok(())
        },
        DryRunOutcome::WouldRevert {
            reason,
        } => {
            error!(
                "verify-fixture: eth_call simulation REVERTED: {reason}\nCommon selectors:\n  - \
                 AttestationProofRejected: the Primary/Fallback verifier rejected the proof \
                 (regen needed — proof bytes don't match the public-instance VK)\n  - \
                 LayerHashesProofRejected: the LayerHashesMovement verifier rejected (same fix)\n  \
                 - VerifyBlockDisabled: bridge was deployed without WIRE_VERIFY_BLOCK=true\n  - \
                 BkSetCommitmentMismatch / BlockSeqNoNotMonotonic / PrevAnchorMismatch: an \
                 on-chain state change landed between the cheap-check read and this simulation"
            );
            std::process::exit(1);
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_prover_daemon(
    state_path: PathBuf,
    proofs_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    an_node_url: Option<String>,
    backoff: BackoffConfig,
    skip_verified_gate: bool,
) -> anyhow::Result<()> {
    let signer: PrivateKeySigner = private_key.parse()?;
    let probe_provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe_provider.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);

    let bridge = Arc::new(EthBridgeClient::new(bridge_address, provider));
    let source =
        Arc::new(ProverProofsBlockSource::new(&proofs_dir).skip_verified_gate(skip_verified_gate));
    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, bridge)?;
    let metrics = RelayerMetrics::new();

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutdown signal received");
    };

    info!(?backoff, proofs_dir = %proofs_dir.display(), "daemon-prover starting");
    let summary = match an_node_url {
        None => {
            relayer
                .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
                .await?
        },
        Some(url) => {
            let sentry = BkSetSentry::from_node_url(url)?;
            let mut guarded = SentryGuardedRelayer::new(relayer, sentry);
            guarded
                .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
                .await?
        },
    };
    info!(?summary, snapshot = ?metrics.snapshot(), "daemon-prover stopped");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn submit_verify_block(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    dry_run: bool,
    skip_verified_gate: bool,
) -> anyhow::Result<()> {
    let source = ProverProofsBlockSource::new(&proofs_dir).skip_verified_gate(skip_verified_gate);
    let block = source
        .fetch(block_seq_no)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no proof bundle for seq_no={block_seq_no}"))?;

    if dry_run {
        let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let bridge = EthBridgeClient::new(bridge_address, provider);
        match bridge.dry_run_block(&block).await? {
            DryRunOutcome::WouldSucceed => info!("dry-run: verifyBlock would succeed"),
            DryRunOutcome::WouldRevert {
                reason,
            } => {
                anyhow::bail!("dry-run reverted: {reason}");
            },
        }
        return Ok(());
    }

    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);

    match bridge.submit_block(&block).await? {
        bridge_relayer_daemon::SubmitOutcome::Verified {
            tx_hash,
            new_state,
        } => {
            info!(?tx_hash, ?new_state, "verifyBlock submitted");
        },
        bridge_relayer_daemon::SubmitOutcome::Reverted {
            reason,
        } => {
            anyhow::bail!("verifyBlock reverted: {reason}");
        },
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_prover_proof(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    simulate: bool,
    skip_verified_gate: bool,
    accept_halo2_proofs: bool,
) -> anyhow::Result<()> {
    let source = ProverProofsBlockSource::new(&proofs_dir)
        .skip_verified_gate(skip_verified_gate)
        .accept_halo2_proofs(accept_halo2_proofs);
    let block = source
        .fetch(block_seq_no)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no proof bundle for seq_no={block_seq_no}"))?;

    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);
    let on_chain = bridge.read_state().await?;

    if block.bk_set_commitment != on_chain.bk_set_commitment {
        anyhow::bail!(
            "bk_set mismatch: proof={} chain={}",
            block.bk_set_commitment,
            on_chain.bk_set_commitment
        );
    }
    if block.block_seq_no <= on_chain.last_seen_block_seq_no {
        anyhow::bail!(
            "seq_no not monotonic: proof={} chain_last_seen={}",
            block.block_seq_no,
            on_chain.last_seen_block_seq_no
        );
    }
    if block.prev_max_level_layer_hash != on_chain.prev_max_level_layer_hash {
        anyhow::bail!(
            "prev anchor mismatch: proof={} chain={}",
            block.prev_max_level_layer_hash,
            on_chain.prev_max_level_layer_hash
        );
    }

    info!("verify-prover-proof: anchor checks PASS");
    if !simulate {
        return Ok(());
    }
    match bridge.dry_run_block(&block).await? {
        DryRunOutcome::WouldSucceed => {
            info!("verify-prover-proof: eth_call PASS");
            Ok(())
        },
        DryRunOutcome::WouldRevert {
            reason,
        } => anyhow::bail!("eth_call reverted: {reason}"),
    }
}

async fn submit_withdraw(
    proof_event: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    dry_run: bool,
) -> anyhow::Result<()> {
    let bundle = PartnerWithdrawalProof::from_json_bytes(&std::fs::read(&proof_event)?)?;
    let proof = bundle.proof_bytes()?;
    let pub_inputs = bundle.public_inputs()?;

    if dry_run {
        let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let bridge = EthBridgeClient::new(bridge_address, provider);
        match bridge.dry_run_withdraw(&proof, &pub_inputs).await? {
            DryRunOutcome::WouldSucceed => info!("dry-run: withdrawByProof would succeed"),
            DryRunOutcome::WouldRevert {
                reason,
            } => {
                anyhow::bail!("dry-run reverted: {reason}");
            },
        }
        return Ok(());
    }

    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);

    match bridge.submit_withdraw(&proof, &pub_inputs).await? {
        WithdrawSubmitOutcome::Paid {
            tx_hash,
        } => info!(?tx_hash, "withdrawByProof paid out"),
        WithdrawSubmitOutcome::Reverted {
            reason,
        } => {
            anyhow::bail!("withdrawByProof reverted: {reason}");
        },
    }
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
