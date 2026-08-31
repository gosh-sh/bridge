//! `eth-lc-relayer` — beacon → prove → `EthBeaconLightClient.submitUpdate`.
//!
//! Subcommands:
//! - `beacon-watch` — fetch `finality_update`, print slots (no prove/submit)
//! - `prove-one` — subprocess `export_step_vk_blob` (needs Hermez SRS + n14
//!   RAM)
//! - `submit-one` / `submit-rotate` — send an existing bundle to AN
//! - `ancestry-one` — parent-hash chain of an epoch vs a checkpoint hash
//! - `daemon` — loop; `--dry-run` mocks AN, `--mock-prove` skips Halo2

use std::{path::PathBuf, sync::Arc, time::Duration};

#[cfg(feature = "live-submit")]
use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use clap::{Parser, Subcommand};
#[cfg(feature = "live-submit")]
use eth_light_client_relayer::AnInterfaceSubmitter;
use eth_light_client_relayer::{
    AnConfig, BackoffConfig, BeaconSource, EthExecutionRpc, HttpBeaconSource, MockAnSubmitter,
    MockProofGenerator, ProofGenerator, Relayer, RelayerConfig, RelayerMetrics, StateLock,
    SubprocessProofGenerator, SubprocessProverConfig,
};
#[cfg(feature = "live-submit")]
use eth_light_client_relayer::{RotateProofBundle, StepProofBundle};
use tracing::info;
#[cfg(feature = "live-submit")]
use tvm_client::crypto::KeyPair;

#[derive(Parser, Debug)]
#[command(
    name = "eth-lc-relayer",
    about = "Ethereum beacon light-client relayer"
)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Fetch and print the latest light-client finality update.
    BeaconWatch {
        #[arg(long, env = "BEACON_URL")]
        beacon_url: String,
    },
    /// Prove one step from the live beacon (or `--finality-json`) into
    /// `--out-dir`.
    ProveOne {
        #[arg(long, env = "BEACON_URL")]
        beacon_url: Option<String>,
        #[arg(long)]
        finality_json: Option<PathBuf>,
        #[arg(long, env = "LIGHT_CLIENT_PROVER_DIR")]
        prover_dir: PathBuf,
        #[arg(long, env = "STEP_SRS_PATH", default_value = "data/kzg_params_19.srs")]
        srs_path: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 7200)]
        timeout_secs: u64,
    },
    /// Submit `submitUpdate` from a bundle directory.
    SubmitOne {
        #[arg(long)]
        bundle_dir: PathBuf,
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: String,
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: String,
        #[arg(long, env = "AN_LC_ABI_PATH")]
        an_lc_abi_path: String,
        #[arg(long, env = "AN_LIGHT_CLIENT")]
        an_light_client: String,
        #[arg(long, env = "AN_SENDER")]
        an_sender: String,
    },
    /// Submit `submitRotate` from a `rotate_tree_n8` EMIT_VKBLOB directory.
    SubmitRotate {
        #[arg(long)]
        bundle_dir: PathBuf,
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: String,
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: String,
        #[arg(long, env = "AN_LC_ABI_PATH")]
        an_lc_abi_path: String,
        #[arg(long, env = "AN_LIGHT_CLIENT")]
        an_light_client: String,
        #[arg(long, env = "AN_SENDER")]
        an_sender: String,
    },
    /// Walk the execution parent-hash chain of a checkpoint epoch.
    AncestryOne {
        #[arg(long, env = "BEACON_URL")]
        beacon_url: String,
        #[arg(long)]
        checkpoint_slot: u64,
        #[arg(long)]
        deposit_hash: String,
    },
    /// Fetch execution header RLPs and call `submitAncestry`.
    SubmitAncestry {
        #[arg(long, env = "ETH_RPC_URL")]
        eth_rpc_url: String,
        #[arg(long)]
        checkpoint_hash: String,
        #[arg(long, default_value_t = 32)]
        max_headers: usize,
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: String,
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: String,
        #[arg(long, env = "AN_LC_ABI_PATH")]
        an_lc_abi_path: String,
        #[arg(long, env = "AN_LIGHT_CLIENT")]
        an_light_client: String,
        #[arg(long, env = "AN_SENDER")]
        an_sender: String,
    },
    /// Long-running fetch → prove → submit loop.
    Daemon {
        #[arg(long, env = "BEACON_URL")]
        beacon_url: String,
        #[arg(long, default_value = "./eth-lc-relayer-state.json")]
        state: PathBuf,
        #[arg(long, env = "LIGHT_CLIENT_PROVER_DIR")]
        prover_dir: Option<PathBuf>,
        #[arg(long, env = "STEP_SRS_PATH")]
        srs_path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        mock_prove: bool,
        /// Do not send to AN (in-memory submitter).
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Attempt `submitRotate` on a period jump (needs n14 + sound opcode).
        #[arg(long, default_value_t = false)]
        enable_rotate: bool,
        #[arg(long, env = "AN_GRAPHQL_URL")]
        an_graphql_url: Option<String>,
        #[arg(long, env = "AN_KEYS_PATH")]
        an_keys_path: Option<String>,
        #[arg(long, env = "AN_LC_ABI_PATH")]
        an_lc_abi_path: Option<String>,
        #[arg(long, env = "AN_LIGHT_CLIENT")]
        an_light_client: Option<String>,
        #[arg(long, env = "AN_SENDER")]
        an_sender: Option<String>,
        #[arg(long, default_value_t = false)]
        allow_insecure_graphql: bool,
        #[arg(long, default_value_t = 8)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 300)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
        #[arg(long, default_value_t = 64)]
        poll_secs: u64,
        #[arg(long, default_value_t = 7200)]
        prove_timeout_secs: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match Args::parse().cmd {
        Cmd::BeaconWatch {
            beacon_url,
        } => beacon_watch(beacon_url).await,
        Cmd::ProveOne {
            beacon_url,
            finality_json,
            prover_dir,
            srs_path,
            out_dir,
            timeout_secs,
        } => {
            prove_one(
                beacon_url,
                finality_json,
                prover_dir,
                srs_path,
                out_dir,
                timeout_secs,
            )
            .await
        },
        Cmd::SubmitOne {
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            submit_one(
                bundle_dir,
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
            )
            .await
        },
        Cmd::SubmitRotate {
            bundle_dir,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            submit_rotate(
                bundle_dir,
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
            )
            .await
        },
        Cmd::AncestryOne {
            beacon_url,
            checkpoint_slot,
            deposit_hash,
        } => ancestry_one(beacon_url, checkpoint_slot, deposit_hash).await,
        Cmd::SubmitAncestry {
            eth_rpc_url,
            checkpoint_hash,
            max_headers,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            submit_ancestry_cmd(
                eth_rpc_url,
                checkpoint_hash,
                max_headers,
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
            )
            .await
        },
        Cmd::Daemon {
            beacon_url,
            state,
            prover_dir,
            srs_path,
            mock_prove,
            dry_run,
            enable_rotate,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
            allow_insecure_graphql,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            poll_secs,
            prove_timeout_secs,
        } => {
            let mut an = AnConfig::default();
            if let Some(v) = an_graphql_url {
                an.graphql_url = v;
            }
            if let Some(v) = an_keys_path {
                an.keys_path = v;
            }
            if let Some(v) = an_lc_abi_path {
                an.light_client_abi_path = v;
            }
            if let Some(v) = an_light_client {
                an.light_client = v;
            }
            if let Some(v) = an_sender {
                an.sender = v;
            }
            run_daemon(
                beacon_url,
                state,
                prover_dir,
                srs_path,
                mock_prove,
                dry_run,
                enable_rotate,
                an,
                allow_insecure_graphql,
                BackoffConfig {
                    initial: Duration::from_secs(backoff_initial_secs),
                    max: Duration::from_secs(backoff_max_secs),
                    multiplier: backoff_multiplier,
                },
                poll_secs,
                prove_timeout_secs,
            )
            .await
        },
    }
}

async fn beacon_watch(beacon_url: String) -> anyhow::Result<()> {
    let src = HttpBeaconSource::new(beacon_url)?;
    let u = src.fetch_finality().await?;
    info!(
        attested_slot = u.attested_slot,
        finalized_slot = u.finalized_slot,
        period = u.period(),
        participation = u.participation,
        exec = %hex::encode(u.execution_block_hash),
        "finality_update"
    );
    Ok(())
}

async fn prove_one(
    beacon_url: Option<String>,
    finality_json: Option<PathBuf>,
    prover_dir: PathBuf,
    srs_path: PathBuf,
    out_dir: PathBuf,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let mut update = if let Some(p) = finality_json {
        eth_light_client_relayer::parse_finality_update(&std::fs::read_to_string(p)?)?
    } else {
        let url = beacon_url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("need --beacon-url or --finality-json"))?;
        HttpBeaconSource::new(url.clone())?.fetch_finality().await?
    };
    if update.committee_json.is_empty() {
        if let Some(url) = &beacon_url {
            let src = HttpBeaconSource::new(url.clone())?;
            let prev = update.period().saturating_sub(1);
            update.committee_json = src.fetch_period_update(prev).await?;
        }
    }
    let gen = SubprocessProofGenerator::new(SubprocessProverConfig {
        prover_dir,
        srs_path,
        timeout: Duration::from_secs(timeout_secs),
    });
    let bundle = gen.generate_step(&update).await?;
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(
        out_dir.join("step_public_inputs.bin"),
        &bundle.public_inputs,
    )?;
    std::fs::write(out_dir.join("step_proof_blake2b.bin"), &bundle.proof)?;
    info!(
        slot = bundle.parsed.finalized_slot,
        proof_len = bundle.proof.len(),
        out = %out_dir.display(),
        "wrote step bundle"
    );
    Ok(())
}

async fn ancestry_one(
    beacon_url: String,
    checkpoint_slot: u64,
    deposit_hash: String,
) -> anyhow::Result<()> {
    let raw = hex::decode(deposit_hash.trim_start_matches("0x"))?;
    let deposit: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("--deposit-hash must be 32 bytes"))?;
    let src = HttpBeaconSource::new(beacon_url)?;
    let checkpoint = src.fetch_exec_link_at_slot(checkpoint_slot).await?;
    let mut links = vec![checkpoint.clone()];
    let start = checkpoint_slot.saturating_sub(31);
    for slot in (start..checkpoint_slot).rev() {
        let want = links.last().unwrap().parent_hash;
        match src.fetch_exec_link_at_slot(slot).await {
            Ok(link) if link.block_hash == want => links.push(link),
            Ok(_) => tracing::debug!(slot, "slot hash is not the next parent"),
            Err(e) => tracing::debug!(slot, %e, "skip slot"),
        }
    }
    eth_light_client_relayer::covers_deposit(checkpoint.block_hash, deposit, &links)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    info!(
        checkpoint = %hex::encode(links[0].block_hash),
        links = links.len(),
        "deposit is on the checkpoint parent-hash chain"
    );
    Ok(())
}

fn live_submit_needs_feature() -> anyhow::Error {
    anyhow::anyhow!("rebuild with `--features live-submit` to submit to AN (or pass --dry-run)")
}

#[cfg(not(feature = "live-submit"))]
async fn submit_one(
    _bundle_dir: PathBuf,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
async fn submit_one(
    bundle_dir: PathBuf,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
    let bundle = StepProofBundle::from_dir(&bundle_dir)?;
    let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an_keys_path)?)?;
    let abi =
        TvmAckiNacki::load_abi(&an_lc_abi_path).map_err(|e| anyhow::anyhow!("load ABI: {e}"))?;
    let tvm = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi: abi,
    })
    .map_err(|e| anyhow::anyhow!("tvm connect: {e}"))?;
    let submitter =
        AnInterfaceSubmitter::new(Arc::new(tvm), eth_light_client_relayer::AnSubmitConfig {
            from: an_sender,
            light_client: an_light_client,
            confirm_timeout_secs: 120,
        });
    match submitter.submit_update(&bundle).await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(tx = ?tx_hash.map(hex::encode), "submitUpdate accepted");
        },
        other => anyhow::bail!("submitUpdate: {other:?}"),
    }
    Ok(())
}

#[cfg(not(feature = "live-submit"))]
async fn submit_rotate(
    _bundle_dir: PathBuf,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
async fn submit_rotate(
    bundle_dir: PathBuf,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
    let bundle = RotateProofBundle::from_dir(&bundle_dir)?;
    let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an_keys_path)?)?;
    let abi =
        TvmAckiNacki::load_abi(&an_lc_abi_path).map_err(|e| anyhow::anyhow!("load ABI: {e}"))?;
    let tvm = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi: abi,
    })
    .map_err(|e| anyhow::anyhow!("tvm connect: {e}"))?;
    let submitter =
        AnInterfaceSubmitter::new(Arc::new(tvm), eth_light_client_relayer::AnSubmitConfig {
            from: an_sender,
            light_client: an_light_client,
            confirm_timeout_secs: 120,
        });
    match submitter.submit_rotate(&bundle).await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(period = bundle.period, tx = ?tx_hash.map(hex::encode), "submitRotate accepted");
        },
        other => anyhow::bail!("submitRotate: {other:?}"),
    }
    Ok(())
}

#[cfg(not(feature = "live-submit"))]
#[allow(clippy::too_many_arguments)]
async fn submit_ancestry_cmd(
    eth_rpc_url: String,
    checkpoint_hash: String,
    max_headers: usize,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    let raw = hex::decode(checkpoint_hash.trim_start_matches("0x"))?;
    let checkpoint: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("--checkpoint-hash must be 32 bytes"))?;
    let headers = EthExecutionRpc::new(eth_rpc_url)?
        .ancestry_headers(checkpoint, max_headers)
        .await?;
    info!(
        n = headers.len(),
        "fetched ancestry headers (rebuild with --features live-submit to submit)"
    );
    let _ = headers;
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
#[allow(clippy::too_many_arguments)]
async fn submit_ancestry_cmd(
    eth_rpc_url: String,
    checkpoint_hash: String,
    max_headers: usize,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
    let raw = hex::decode(checkpoint_hash.trim_start_matches("0x"))?;
    let checkpoint: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("--checkpoint-hash must be 32 bytes"))?;
    let headers = EthExecutionRpc::new(eth_rpc_url)?
        .ancestry_headers(checkpoint, max_headers)
        .await?;
    let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an_keys_path)?)?;
    let abi =
        TvmAckiNacki::load_abi(&an_lc_abi_path).map_err(|e| anyhow::anyhow!("load ABI: {e}"))?;
    let tvm = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi: abi,
    })
    .map_err(|e| anyhow::anyhow!("tvm connect: {e}"))?;
    let submitter =
        AnInterfaceSubmitter::new(Arc::new(tvm), eth_light_client_relayer::AnSubmitConfig {
            from: an_sender,
            light_client: an_light_client,
            confirm_timeout_secs: 120,
        });
    match submitter.submit_ancestry(&headers).await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(n = headers.len(), tx = ?tx_hash.map(hex::encode), "submitAncestry accepted");
        },
        other => anyhow::bail!("submitAncestry: {other:?}"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon(
    beacon_url: String,
    state_path: PathBuf,
    prover_dir: Option<PathBuf>,
    srs_path: Option<PathBuf>,
    mock_prove: bool,
    dry_run: bool,
    enable_rotate: bool,
    an: AnConfig,
    allow_insecure: bool,
    backoff: BackoffConfig,
    poll_secs: u64,
    prove_timeout_secs: u64,
) -> anyhow::Result<()> {
    backoff.validate().map_err(|e| anyhow::anyhow!(e))?;
    let _lock = StateLock::acquire(&state_path)?;
    let source = Arc::new(HttpBeaconSource::new(beacon_url)?);
    let mut cfg = RelayerConfig::new(state_path.clone());
    cfg.poll_interval = Duration::from_secs(poll_secs);
    cfg.enable_rotate = enable_rotate;

    if dry_run {
        info!("dry-run: MockAnSubmitter (no AN tx)");
        if mock_prove {
            let relayer = Relayer::new(
                cfg,
                source,
                Arc::new(MockProofGenerator::new()),
                Arc::new(MockAnSubmitter::accepting()),
            )?;
            return run(relayer, backoff).await;
        }
        let prover_dir = prover_dir
            .ok_or_else(|| anyhow::anyhow!("--prover-dir required unless --mock-prove"))?;
        let gen = SubprocessProofGenerator::new(SubprocessProverConfig {
            prover_dir,
            srs_path: srs_path.unwrap_or_else(|| PathBuf::from("data/kzg_params_19.srs")),
            timeout: Duration::from_secs(prove_timeout_secs),
        });
        let relayer = Relayer::new(
            cfg,
            source,
            Arc::new(gen),
            Arc::new(MockAnSubmitter::accepting()),
        )?;
        return run(relayer, backoff).await;
    }

    #[cfg(not(feature = "live-submit"))]
    {
        let _ = (
            an,
            allow_insecure,
            mock_prove,
            prover_dir,
            srs_path,
            cfg,
            source,
        );
        Err(live_submit_needs_feature())
    }

    #[cfg(feature = "live-submit")]
    {
        an.validate_live_graphql_endpoint(allow_insecure)
            .map_err(|e| anyhow::anyhow!(e))?;
        if !an.is_live_submit_ready() {
            anyhow::bail!(
                "live submit needs AN_GRAPHQL_URL, AN_KEYS_PATH, AN_LC_ABI_PATH, AN_LIGHT_CLIENT, \
                 AN_SENDER (or pass --dry-run)"
            );
        }
        let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an.keys_path)?)?;
        let abi = TvmAckiNacki::load_abi(&an.light_client_abi_path)
            .map_err(|e| anyhow::anyhow!("load ABI: {e}"))?;
        let tvm = TvmAckiNacki::connect(TvmClientConfig {
            graphql_endpoints: vec![an.graphql_url.clone()],
            keys,
            bridge_abi: abi,
        })
        .map_err(|e| anyhow::anyhow!("tvm connect: {e}"))?;
        let submitter = Arc::new(AnInterfaceSubmitter::new(
            Arc::new(tvm),
            an.to_submit_config(),
        ));

        if mock_prove {
            let relayer =
                Relayer::new(cfg, source, Arc::new(MockProofGenerator::new()), submitter)?;
            return run(relayer, backoff).await;
        }
        let prover_dir = prover_dir
            .ok_or_else(|| anyhow::anyhow!("--prover-dir required unless --mock-prove"))?;
        let gen = SubprocessProofGenerator::new(SubprocessProverConfig {
            prover_dir,
            srs_path: srs_path.unwrap_or_else(|| PathBuf::from("data/kzg_params_19.srs")),
            timeout: Duration::from_secs(prove_timeout_secs),
        });
        let relayer = Relayer::new(cfg, source, Arc::new(gen), submitter)?;
        return run(relayer, backoff).await;
    }
}

async fn run<S, P, A>(mut relayer: Relayer<S, P, A>, backoff: BackoffConfig) -> anyhow::Result<()>
where
    S: eth_light_client_relayer::BeaconSource + 'static,
    P: eth_light_client_relayer::ProofGenerator + 'static,
    A: eth_light_client_relayer::AnSubmitter + 'static,
{
    info!(
        last_slot = ?relayer.state().last_finalized_slot,
        "starting light-client daemon"
    );
    let metrics = RelayerMetrics::new();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    relayer
        .run_until_shutdown(backoff, metrics, shutdown)
        .await
        .map_err(Into::into)
}
