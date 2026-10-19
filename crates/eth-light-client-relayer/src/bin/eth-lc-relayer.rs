//! `eth-lc-relayer` — beacon → prove → `EthBeaconLightClient.submitUpdate`.
//!
//! Subcommands:
//! - `beacon-watch` — fetch `finality_update`, print slots (no prove/submit)
//! - `prove-one` — subprocess `export_step_vk_blob` (needs Hermez SRS + n14
//!   RAM)
//! - `submit-one` / `submit-rotate` / `flip-owner` — send to AN
//! - `set-committee` — owner `setCommitteeCommitment` from a proven bundle
//!   (weak-subjectivity bootstrap / manual hop while `--no-rotate`)
//! - `set-gloas-fork-slot` — owner `setGloasForkSlot`; required once after
//!   deploy or `submitUpdate` / `submitRotate` revert 256
//! - `ancestry-one` — parent-hash chain of an epoch vs a checkpoint hash
//! - `daemon` — loop; `--dry-run` mocks AN, `--mock-prove` skips Halo2,
//!   `--no-rotate` opts out of rotation, `--flip-owner` opts in to the owner
//!   flip. `ETH_RPC_URL` fetches the span between the previous checkpoint and
//!   the new one after each accepted checkpoint and runs `link_headers`
//!   locally. On-chain `submitAncestry` stays off unless `--submit-ancestry` is
//!   set.

use std::{path::PathBuf, sync::Arc, time::Duration};

#[cfg(feature = "live-submit")]
use acki_nacki_interface::{TvmAckiNacki, TvmClientConfig};
use clap::{Parser, Subcommand};
#[cfg(feature = "live-submit")]
use eth_light_client_relayer::AnInterfaceSubmitter;
#[cfg(feature = "live-submit")]
use eth_light_client_relayer::RelayerState;
use eth_light_client_relayer::{
    AnConfig, AnSubmitter, BackoffConfig, BeaconSource, EthExecutionRpc, HttpBeaconSource,
    MockAnSubmitter, MockProofGenerator, ProofGenerator, Relayer, RelayerConfig, RelayerMetrics,
    StateLock, SubprocessProofGenerator, SubprocessProverConfig,
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
        /// Prebuilt `export_step_vk_blob` example (`cargo build --release
        /// --example export_step_vk_blob` in `eth-light-client-prover/`).
        #[arg(long, env = "LIGHT_CLIENT_STEP_PROVER_BIN")]
        step_prover_bin: PathBuf,
        /// Working directory for the prover; a relative `--srs-path` resolves
        /// against it, and the SRS is checked to be there at startup.
        /// Defaults to the binary's own directory.
        #[arg(long, env = "LIGHT_CLIENT_PROVER_DIR")]
        prover_dir: Option<PathBuf>,
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
    /// Owner `setCommitteeCommitment(commitment, period)` from a proven step
    /// bundle (public-input word 5 = commitment of the committee that signed
    /// the attested header). Bootstraps a fresh contract and hops periods
    /// while rotate is off; records the period in `--state` so the daemon
    /// does not re-flag it. Refused by the contract after
    /// `disableOwnerRotation`.
    SetCommittee {
        #[arg(long)]
        bundle_dir: PathBuf,
        /// Override the period (default: attested slot of the bundle / 8192).
        #[arg(long)]
        period: Option<u64>,
        #[arg(long, default_value = "./eth-lc-relayer-state.json")]
        state: PathBuf,
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
    /// Owner `setGloasForkSlot(slot)`. After `deployLightClient` (or
    /// `updateCode` with an empty user cell) both `submitUpdate` and
    /// `submitRotate` revert 256 until this runs. Sepolia: `11296768`.
    /// Mainnet (Gloas unscheduled): `18446744073709551615`.
    SetGloasForkSlot {
        #[arg(long)]
        slot: u64,
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
    /// Records the period in `--state` so a stopped daemon resumes; a
    /// running daemon reads `getCommitteeState` on the next tick.
    SubmitRotate {
        #[arg(long)]
        bundle_dir: PathBuf,
        #[arg(long, default_value = "./eth-lc-relayer-state.json")]
        state: PathBuf,
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
    /// Fetch the execution header RLPs between two proven anchors and call
    /// `submitAncestry`. `--checkpoint-hash` is the newer anchor,
    /// `--older-anchor-hash` the older one; both must already be proven.
    SubmitAncestry {
        #[arg(long, env = "ETH_RPC_URL")]
        eth_rpc_url: String,
        #[arg(long)]
        checkpoint_hash: String,
        #[arg(long)]
        older_anchor_hash: String,
        #[arg(long, default_value_t = 33)]
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
    /// Owner flip: check `getLightClient(l1ChainId)`, then
    /// `disableOwnerAnchorsOn(chainId)` + `disableOwnerRotation`. Relayer keys
    /// must be the owner pubkey. Afterwards the light client is the only
    /// anchor writer, and it anchors one checkpoint block per epoch and no
    /// L2.
    FlipOwner {
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
        #[arg(long, env = "AN_USDC_BRIDGE")]
        an_usdc_bridge: String,
        #[arg(long, env = "AN_USDC_ABI_PATH")]
        an_usdc_abi_path: String,
    },
    /// Long-running fetch → prove → submit loop.
    Daemon {
        #[arg(long, env = "BEACON_URL")]
        beacon_url: String,
        #[arg(long, default_value = "./eth-lc-relayer-state.json")]
        state: PathBuf,
        /// Prebuilt `export_step_vk_blob` example; required unless
        /// `--mock-prove`. Checked to exist and be executable at startup.
        #[arg(long, env = "LIGHT_CLIENT_STEP_PROVER_BIN")]
        step_prover_bin: Option<PathBuf>,
        /// Prebuilt `rotate_tree_n8` example (`--features aggregation`);
        /// required unless `--no-rotate` or `--mock-prove`, and ignored with
        /// `--no-rotate`.
        #[arg(long, env = "LIGHT_CLIENT_ROTATE_PROVER_BIN")]
        rotate_prover_bin: Option<PathBuf>,
        /// Working directory for the provers. Required when the daemon may
        /// rotate: `rotate_tree_n8` reads its shard snarks (`out/shard_snark`)
        /// and the outer SRS (`../params/`) there, and the period check reads
        /// `fixtures/mainnet/` there. Otherwise optional: a relative
        /// `--srs-path` resolves against it, and the SRS is checked to be
        /// there at startup. Defaults to the binary's own directory.
        #[arg(long, env = "LIGHT_CLIENT_PROVER_DIR")]
        prover_dir: Option<PathBuf>,
        #[arg(long, env = "STEP_SRS_PATH")]
        srs_path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        mock_prove: bool,
        /// Do not send to AN (in-memory submitter).
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Skip `submitRotate` on a period jump. Rotate is **on** by default
        /// (tvm-sdk#284 co-deploys with this contract).
        #[arg(long, default_value_t = false)]
        no_rotate: bool,
        /// After the first accepted `submitUpdate`, run the owner flip
        /// (`disableOwnerAnchors` + `disableOwnerRotation`). Default **off**.
        /// Needs `AN_USDC_BRIDGE` and `AN_USDC_ABI_PATH`.
        #[arg(long, default_value_t = false, conflicts_with = "no_flip_owner")]
        flip_owner: bool,
        /// No owner flip. This is the default; the flag is kept so existing
        /// units keep starting.
        #[arg(long, default_value_t = false)]
        no_flip_owner: bool,
        /// Execution JSON-RPC. When set, each accepted `submitUpdate` fetches
        /// the span between the previous checkpoint and the new one and runs
        /// `link_headers` locally. Does **not** call `submitAncestry` unless
        /// `--submit-ancestry` is also set.
        #[arg(long, env = "ETH_RPC_URL")]
        eth_rpc_url: Option<String>,
        /// Send `submitAncestry` after each checkpoint. Default **off**. The
        /// span needs a proven anchor at both ends, so nothing is sent until a
        /// previous checkpoint is on record, nor when the gap to it is wider
        /// than a span. Needs `--eth-rpc-url`.
        #[arg(long, env = "SUBMIT_ANCESTRY", default_value_t = false)]
        submit_ancestry: bool,
        /// With `--no-rotate`: on a period jump, prove a step of the new
        /// period and advance the committee with the owner key
        /// (`setCommitteeCommitment`) instead of waiting for a rotate proof.
        /// Shadow only; refused by the contract after `disableOwnerRotation`.
        #[arg(long, default_value_t = false, requires = "no_rotate")]
        owner_hop: bool,
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
        #[arg(long, env = "AN_USDC_BRIDGE")]
        an_usdc_bridge: Option<String>,
        #[arg(long, env = "AN_USDC_ABI_PATH")]
        an_usdc_abi_path: Option<String>,
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
            step_prover_bin,
            prover_dir,
            srs_path,
            out_dir,
            timeout_secs,
        } => {
            prove_one(
                beacon_url,
                finality_json,
                step_prover_bin,
                prover_dir,
                srs_path,
                out_dir,
                timeout_secs,
            )
            .await
        },
        Cmd::SetCommittee {
            bundle_dir,
            period,
            state,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            set_committee(
                bundle_dir,
                period,
                state,
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
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
        Cmd::SetGloasForkSlot {
            slot,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            set_gloas_fork_slot(
                slot,
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
            state,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
        } => {
            submit_rotate(
                bundle_dir,
                state,
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
            older_anchor_hash,
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
                older_anchor_hash,
                max_headers,
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
            )
            .await
        },
        Cmd::FlipOwner {
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
            an_usdc_bridge,
            an_usdc_abi_path,
        } => {
            flip_owner_cmd(
                an_graphql_url,
                an_keys_path,
                an_lc_abi_path,
                an_light_client,
                an_sender,
                an_usdc_bridge,
                an_usdc_abi_path,
            )
            .await
        },
        Cmd::Daemon {
            beacon_url,
            state,
            step_prover_bin,
            rotate_prover_bin,
            prover_dir,
            srs_path,
            mock_prove,
            dry_run,
            no_rotate,
            flip_owner,
            no_flip_owner,
            eth_rpc_url,
            submit_ancestry,
            owner_hop,
            an_graphql_url,
            an_keys_path,
            an_lc_abi_path,
            an_light_client,
            an_sender,
            an_usdc_bridge,
            an_usdc_abi_path,
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
            if let Some(v) = an_usdc_bridge {
                an.usdc_bridge = v;
            }
            if let Some(v) = an_usdc_abi_path {
                an.usdc_abi_path = v;
            }
            run_daemon(
                beacon_url,
                state,
                step_prover_bin,
                rotate_prover_bin,
                prover_dir,
                srs_path,
                mock_prove,
                dry_run,
                !no_rotate,
                flip_owner && !no_flip_owner,
                eth_rpc_url,
                submit_ancestry,
                owner_hop,
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
    let (fork, gvr) = match &u.chain {
        Some(c) => (c.fork_version_hex(), c.genesis_validators_root_hex()),
        None => ("?".into(), "?".into()),
    };
    info!(
        attested_slot = u.attested_slot,
        finalized_slot = u.finalized_slot,
        signature_slot = u.signature_slot,
        period = u.period(),
        signing_period = u.signing_period(),
        participation = u.participation,
        exec = %hex::encode(u.execution_block_hash),
        fork_version = %fork,
        genesis_validators_root = %gvr,
        "finality_update"
    );
    Ok(())
}

#[cfg(not(feature = "live-submit"))]
#[allow(clippy::too_many_arguments)]
async fn set_committee(
    _bundle_dir: PathBuf,
    _period: Option<u64>,
    _state: PathBuf,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
#[allow(clippy::too_many_arguments)]
async fn set_committee(
    bundle_dir: PathBuf,
    period: Option<u64>,
    state: PathBuf,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
    use eth_light_client_relayer::{RelayerState, SLOTS_PER_SYNC_PERIOD};

    let bundle = StepProofBundle::from_dir(&bundle_dir)?;
    let commitment = bundle.parsed.committee_commitment;
    if commitment == [0u8; 32] {
        anyhow::bail!("bundle public inputs carry a zero committee commitment");
    }
    let period = period.unwrap_or(bundle.parsed.attested_slot / SLOTS_PER_SYNC_PERIOD);
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
            usdc_bridge: None,
        });
    info!(
        period,
        attested_slot = bundle.parsed.attested_slot,
        commitment = %eth_light_client_relayer::le_word_to_uint256_hex(&commitment),
        "setCommitteeCommitment"
    );
    match submitter
        .set_committee_commitment(commitment, period)
        .await?
    {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(tx = ?tx_hash.map(hex::encode), "setCommitteeCommitment accepted");
        },
        other => anyhow::bail!("setCommitteeCommitment: {other:?}"),
    }
    let mut st = RelayerState::load(&state)?.unwrap_or_default();
    st.last_committee_period = Some(period);
    st.rotate_pending_period = None;
    st.save(&state)?;
    info!(state = %state.display(), period, "state file records the committee period");
    Ok(())
}

#[cfg(not(feature = "live-submit"))]
#[allow(clippy::too_many_arguments)]
async fn set_gloas_fork_slot(
    _slot: u64,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
async fn set_gloas_fork_slot(
    slot: u64,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
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
            usdc_bridge: None,
        });
    info!(slot, "setGloasForkSlot");
    match submitter.set_gloas_fork_slot(slot).await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(tx = ?tx_hash.map(hex::encode), "setGloasForkSlot accepted");
        },
        other => anyhow::bail!("setGloasForkSlot: {other:?}"),
    }
    Ok(())
}

/// The subprocess prover for `daemon` and `prove-one`, every binary checked
/// to exist and be executable before anything else starts, and the SRS to be
/// where the step prover will read it: a wrong path is a configuration
/// error, found now rather than at the first proof. The rotate prover, and
/// with it `--prover-dir`, is only needed when the daemon may rotate.
fn subprocess_prover_config(
    step_prover_bin: Option<PathBuf>,
    rotate_prover_bin: Option<PathBuf>,
    prover_dir: Option<PathBuf>,
    srs_path: Option<PathBuf>,
    enable_rotate: bool,
    timeout_secs: u64,
    log_dir: PathBuf,
) -> anyhow::Result<SubprocessProverConfig> {
    use eth_light_client_relayer::prover::require_executable;
    let step_prover_bin = step_prover_bin.ok_or_else(|| {
        anyhow::anyhow!(
            "--step-prover-bin (LIGHT_CLIENT_STEP_PROVER_BIN) required unless --mock-prove"
        )
    })?;
    // With `--no-rotate` nothing rotates: a rotate binary named in a shared
    // env file is neither needed nor checked.
    let rotate_prover_bin = match (rotate_prover_bin, enable_rotate) {
        (_, false) => None,
        (Some(bin), true) => Some(require_executable(&bin, "rotate prover")?),
        (None, true) => anyhow::bail!(
            "--rotate-prover-bin (LIGHT_CLIENT_ROTATE_PROVER_BIN) required unless --no-rotate or \
             --mock-prove"
        ),
    };
    let prover_dir = match prover_dir {
        Some(dir) if dir.is_dir() => Some(dir),
        Some(dir) => anyhow::bail!("--prover-dir {} is not a directory", dir.display()),
        // `rotate_tree_n8` reads its shard snarks (`out/shard_snark`) and the
        // outer SRS (`../params/`) in its working directory, and the period
        // check before it reads `fixtures/mainnet/` there: without the
        // directory the first period jump fails, not the start.
        None if rotate_prover_bin.is_some() => anyhow::bail!(
            "--prover-dir (LIGHT_CLIENT_PROVER_DIR) required unless --no-rotate: rotate_tree_n8 \
             reads its shard snarks and outer SRS there, and the period check its fixture"
        ),
        None => None,
    };
    let cfg = SubprocessProverConfig {
        step_prover_bin: require_executable(&step_prover_bin, "step prover")?,
        rotate_prover_bin,
        prover_dir,
        srs_path: srs_path.unwrap_or_else(|| PathBuf::from("data/kzg_params_19.srs")),
        timeout: Duration::from_secs(timeout_secs),
        log_dir: Some(log_dir),
    };
    // Where the step prover will look for the SRS: a relative path, in its
    // working directory. Checked now, not at the first proof.
    let srs = cfg.step_srs_path();
    anyhow::ensure!(
        srs.is_file(),
        "--srs-path (STEP_SRS_PATH) {}: not a file; a relative path is read in the step prover's \
         working directory",
        srs.display()
    );
    Ok(cfg)
}

async fn prove_one(
    beacon_url: Option<String>,
    finality_json: Option<PathBuf>,
    step_prover_bin: PathBuf,
    prover_dir: Option<PathBuf>,
    srs_path: PathBuf,
    out_dir: PathBuf,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let prover_cfg = subprocess_prover_config(
        Some(step_prover_bin),
        None,
        prover_dir,
        Some(srs_path),
        false,
        timeout_secs,
        out_dir.clone(),
    )?;
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
    let gen = SubprocessProofGenerator::new(prover_cfg);
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
    // From Gloas the checkpoint anchors its bid's parent, which an earlier
    // slot produced, so the walk starts at the anchored hash, not at this
    // slot's own block, and reaches one slot further back.
    let anchor = src.fetch_anchored_exec_hash(checkpoint_slot).await?;
    let mut links: Vec<eth_light_client_relayer::ExecLink> = Vec::new();
    let mut want = anchor;
    let start = checkpoint_slot.saturating_sub(32);
    for slot in (start..=checkpoint_slot).rev() {
        if links.len() == 32 {
            break;
        }
        match src.fetch_exec_link_at_slot(slot).await {
            Ok(link) if link.block_hash == want => {
                want = link.parent_hash;
                links.push(link);
            },
            Ok(_) => tracing::debug!(slot, "slot hash is not the next parent"),
            Err(e) => tracing::debug!(slot, %e, "skip slot"),
        }
    }
    eth_light_client_relayer::covers_deposit(anchor, deposit, &links)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    info!(
        checkpoint = %hex::encode(links[0].block_hash),
        links = links.len(),
        "deposit is on the checkpoint parent-hash chain"
    );
    Ok(())
}

#[cfg(not(feature = "live-submit"))]
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
            usdc_bridge: None,
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
    _state: PathBuf,
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
    state: PathBuf,
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
            usdc_bridge: None,
        });
    match submitter.submit_rotate(&bundle).await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(period = bundle.period, tx = ?tx_hash.map(hex::encode), "submitRotate accepted");
        },
        other => anyhow::bail!("submitRotate: {other:?}"),
    }
    let mut st = RelayerState::load(&state)?.unwrap_or_default();
    st.last_committee_period = Some(bundle.period);
    st.rotate_pending_period = None;
    st.save(&state)?;
    info!(
        state = %state.display(),
        period = bundle.period,
        "state file records the committee period"
    );
    Ok(())
}

fn parse_anchor_pair(newer_hex: &str, older_hex: &str) -> anyhow::Result<([u8; 32], [u8; 32])> {
    let newer: [u8; 32] = hex::decode(newer_hex.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("--checkpoint-hash must be 32 bytes"))?;
    let older: [u8; 32] = hex::decode(older_hex.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("--older-anchor-hash must be 32 bytes"))?;
    Ok((newer, older))
}

#[cfg(not(feature = "live-submit"))]
#[allow(clippy::too_many_arguments)]
async fn submit_ancestry_cmd(
    eth_rpc_url: String,
    checkpoint_hash: String,
    older_anchor_hash: String,
    max_headers: usize,
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
) -> anyhow::Result<()> {
    let (checkpoint, older) = parse_anchor_pair(&checkpoint_hash, &older_anchor_hash)?;
    let headers = EthExecutionRpc::new(eth_rpc_url)?
        .ancestry_span(checkpoint, older, max_headers)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no span joins these two anchors: --older-anchor-hash is further back than \
                 --max-headers allows, is not an ancestor of --checkpoint-hash, or the two are \
                 adjacent with nothing between them"
            )
        })?;
    info!(
        n = headers.len(),
        "fetched ancestry span (rebuild with --features live-submit to submit)"
    );
    let _ = headers;
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
#[allow(clippy::too_many_arguments)]
async fn submit_ancestry_cmd(
    eth_rpc_url: String,
    checkpoint_hash: String,
    older_anchor_hash: String,
    max_headers: usize,
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
) -> anyhow::Result<()> {
    let (checkpoint, older) = parse_anchor_pair(&checkpoint_hash, &older_anchor_hash)?;
    let headers = EthExecutionRpc::new(eth_rpc_url)?
        .ancestry_span(checkpoint, older, max_headers)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no span joins these two anchors: --older-anchor-hash is further back than \
                 --max-headers allows, is not an ancestor of --checkpoint-hash, or the two are \
                 adjacent with nothing between them"
            )
        })?;
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
            usdc_bridge: None,
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

#[cfg(not(feature = "live-submit"))]
#[allow(clippy::too_many_arguments)]
async fn flip_owner_cmd(
    _an_graphql_url: String,
    _an_keys_path: String,
    _an_lc_abi_path: String,
    _an_light_client: String,
    _an_sender: String,
    _an_usdc_bridge: String,
    _an_usdc_abi_path: String,
) -> anyhow::Result<()> {
    Err(live_submit_needs_feature())
}

#[cfg(feature = "live-submit")]
#[allow(clippy::too_many_arguments)]
async fn flip_owner_cmd(
    an_graphql_url: String,
    an_keys_path: String,
    an_lc_abi_path: String,
    an_light_client: String,
    an_sender: String,
    an_usdc_bridge: String,
    an_usdc_abi_path: String,
) -> anyhow::Result<()> {
    let keys: KeyPair = serde_json::from_str(&std::fs::read_to_string(&an_keys_path)?)?;
    let lc_abi =
        TvmAckiNacki::load_abi(&an_lc_abi_path).map_err(|e| anyhow::anyhow!("load LC ABI: {e}"))?;
    let usdc_abi = TvmAckiNacki::load_abi(&an_usdc_abi_path)
        .map_err(|e| anyhow::anyhow!("load USDC ABI: {e}"))?;
    let tvm_lc = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url.clone()],
        keys: keys.clone(),
        bridge_abi: lc_abi,
    })
    .map_err(|e| anyhow::anyhow!("tvm connect LC: {e}"))?;
    let tvm_usdc = TvmAckiNacki::connect(TvmClientConfig {
        graphql_endpoints: vec![an_graphql_url],
        keys,
        bridge_abi: usdc_abi,
    })
    .map_err(|e| anyhow::anyhow!("tvm connect USDC: {e}"))?;
    let submitter =
        AnInterfaceSubmitter::new(Arc::new(tvm_lc), eth_light_client_relayer::AnSubmitConfig {
            from: an_sender,
            light_client: an_light_client,
            confirm_timeout_secs: 120,
            usdc_bridge: Some(an_usdc_bridge.clone()),
        })
        .with_usdc(an_usdc_bridge, Arc::new(tvm_usdc));
    match submitter.flip_owner().await? {
        eth_light_client_relayer::SubmitOutcome::Accepted {
            tx_hash,
        } => {
            info!(tx = ?tx_hash.map(hex::encode), "flip-owner accepted");
        },
        other => anyhow::bail!("flip-owner: {other:?}"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon(
    beacon_url: String,
    state_path: PathBuf,
    step_prover_bin: Option<PathBuf>,
    rotate_prover_bin: Option<PathBuf>,
    prover_dir: Option<PathBuf>,
    srs_path: Option<PathBuf>,
    mock_prove: bool,
    dry_run: bool,
    enable_rotate: bool,
    flip_owner: bool,
    eth_rpc_url: Option<String>,
    submit_ancestry: bool,
    owner_hop: bool,
    an: AnConfig,
    allow_insecure: bool,
    backoff: BackoffConfig,
    poll_secs: u64,
    prove_timeout_secs: u64,
) -> anyhow::Result<()> {
    backoff.validate().map_err(|e| anyhow::anyhow!(e))?;
    let prover_cfg = if mock_prove {
        None
    } else {
        Some(subprocess_prover_config(
            step_prover_bin,
            rotate_prover_bin,
            prover_dir,
            srs_path,
            enable_rotate,
            prove_timeout_secs,
            prover_log_dir(&state_path),
        )?)
    };
    let _lock = StateLock::acquire(&state_path)?;
    let source = Arc::new(HttpBeaconSource::new(beacon_url)?);
    let mut cfg = RelayerConfig::new(state_path.clone());
    cfg.poll_interval = Duration::from_secs(poll_secs);
    cfg.enable_rotate = enable_rotate;
    cfg.flip_owner = flip_owner;
    cfg.owner_hop = owner_hop;
    cfg.submit_ancestry = submit_ancestry;

    if dry_run {
        info!("dry-run: MockAnSubmitter (no AN tx); Gloas slot from the beacon spec");
        cfg.pin_fork_slot_from_beacon = true;
        if mock_prove {
            let relayer = Relayer::new(
                cfg,
                source,
                Arc::new(MockProofGenerator::new()),
                Arc::new(MockAnSubmitter::accepting()),
            )?;
            return run(
                attach_execution(relayer, eth_rpc_url, submit_ancestry)?,
                backoff,
            )
            .await;
        }
        let gen = SubprocessProofGenerator::new(prover_cfg.clone().expect("checked above"));
        let relayer = Relayer::new(
            cfg,
            source,
            Arc::new(gen),
            Arc::new(MockAnSubmitter::accepting()),
        )?;
        return run(
            attach_execution(relayer, eth_rpc_url, submit_ancestry)?,
            backoff,
        )
        .await;
    }

    #[cfg(not(feature = "live-submit"))]
    {
        let _ = (an, allow_insecure, prover_cfg, cfg, source, eth_rpc_url);
        Err(live_submit_needs_feature())
    }

    #[cfg(feature = "live-submit")]
    {
        an.validate_live_graphql_endpoint(allow_insecure)
            .map_err(|e| anyhow::anyhow!(e))?;
        an.validate_flip_owner(cfg.flip_owner)
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
            keys: keys.clone(),
            bridge_abi: abi,
        })
        .map_err(|e| anyhow::anyhow!("tvm connect: {e}"))?;
        let mut submitter = AnInterfaceSubmitter::new(Arc::new(tvm), an.to_submit_config());
        if an.is_usdc_ready() {
            let usdc_abi = TvmAckiNacki::load_abi(&an.usdc_abi_path)
                .map_err(|e| anyhow::anyhow!("load USDC ABI: {e}"))?;
            let tvm_usdc = TvmAckiNacki::connect(TvmClientConfig {
                graphql_endpoints: vec![an.graphql_url.clone()],
                keys,
                bridge_abi: usdc_abi,
            })
            .map_err(|e| anyhow::anyhow!("tvm connect USDC: {e}"))?;
            submitter = submitter.with_usdc(an.usdc_bridge.clone(), Arc::new(tvm_usdc));
        }
        let submitter = Arc::new(submitter);

        if mock_prove {
            let relayer =
                Relayer::new(cfg, source, Arc::new(MockProofGenerator::new()), submitter)?;
            return run(
                attach_execution(relayer, eth_rpc_url, submit_ancestry)?,
                backoff,
            )
            .await;
        }
        let gen = SubprocessProofGenerator::new(prover_cfg.clone().expect("checked above"));
        let relayer = Relayer::new(cfg, source, Arc::new(gen), submitter)?;
        run(
            attach_execution(relayer, eth_rpc_url, submit_ancestry)?,
            backoff,
        )
        .await
    }
}

fn attach_execution<S, P, A>(
    relayer: Relayer<S, P, A>,
    eth_rpc_url: Option<String>,
    submit_ancestry: bool,
) -> anyhow::Result<Relayer<S, P, A>>
where
    S: BeaconSource + 'static,
    P: ProofGenerator + 'static,
    A: AnSubmitter + 'static,
{
    match eth_rpc_url.filter(|u| !u.is_empty()) {
        Some(url) => {
            if submit_ancestry {
                info!(
                    %url,
                    "ETH_RPC_URL set with --submit-ancestry: daemon will send submitAncestry \
                     for the span between consecutive checkpoints"
                );
            } else {
                info!(
                    %url,
                    "ETH_RPC_URL set: local link_headers after each checkpoint; \
                     on-chain submitAncestry stays off (--submit-ancestry)"
                );
            }
            Ok(relayer.with_execution(Arc::new(EthExecutionRpc::new(url)?)))
        },
        None => {
            if submit_ancestry {
                tracing::warn!(
                    "--submit-ancestry set without --eth-rpc-url; on-chain ancestry cannot run"
                );
            }
            info!("ETH_RPC_URL unset: checkpoints only (rePushAnchor still runs)");
            Ok(relayer)
        },
    }
}

/// `<state file dir>/prover-logs`: prover transcripts of the daemon's proves.
fn prover_log_dir(state: &std::path::Path) -> PathBuf {
    state
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prover-logs")
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

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    /// A file `require_executable` accepts; nothing runs it.
    fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn config(
        dir: &Path,
        rotate: Option<PathBuf>,
        prover_dir: Option<PathBuf>,
        srs: Option<PathBuf>,
        enable_rotate: bool,
    ) -> anyhow::Result<SubprocessProverConfig> {
        subprocess_prover_config(
            Some(executable(dir, "step")),
            rotate,
            prover_dir,
            srs,
            enable_rotate,
            1,
            dir.join("logs"),
        )
    }

    #[test]
    fn rotate_needs_a_prover_dir_and_no_rotate_ignores_the_rotate_binary() {
        let dir = tempfile::tempdir().unwrap();
        let srs = dir.path().join("srs.bin");
        std::fs::write(&srs, "srs").unwrap();
        let rotate = executable(dir.path(), "rotate");
        let err = config(
            dir.path(),
            Some(rotate.clone()),
            None,
            Some(srs.clone()),
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--prover-dir"), "{err}");
        let cfg = config(
            dir.path(),
            Some(rotate.clone()),
            Some(dir.path().to_path_buf()),
            Some(srs.clone()),
            true,
        )
        .unwrap();
        assert_eq!(cfg.rotate_prover_bin, Some(rotate.canonicalize().unwrap()));
        // `--no-rotate`: the binary a shared env file names is neither
        // needed nor checked, and the provers run where they are.
        let cfg = config(
            dir.path(),
            Some(dir.path().join("not-built")),
            None,
            Some(srs),
            false,
        )
        .unwrap();
        assert_eq!(cfg.rotate_prover_bin, None);
        assert_eq!(cfg.prover_dir, None);
    }

    #[test]
    fn the_srs_is_checked_where_the_step_prover_will_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(work.join("data")).unwrap();
        std::fs::write(work.join("data/kzg_params_19.srs"), "srs").unwrap();
        let step = executable(&bin_dir, "step");
        let logs = dir.path().join("logs");
        // The default, relative path: found under `--prover-dir`...
        let cfg = subprocess_prover_config(
            Some(step.clone()),
            None,
            Some(work.clone()),
            None,
            false,
            1,
            logs.clone(),
        )
        .unwrap();
        assert_eq!(cfg.srs_path, PathBuf::from("data/kzg_params_19.srs"));
        // ...and not in the binary's own directory, where the prover would
        // look without one.
        let err =
            subprocess_prover_config(Some(step.clone()), None, None, None, false, 1, logs.clone())
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("--srs-path") && err.contains("bin/data/kzg_params_19.srs"),
            "{err}"
        );
        // An absolute path is taken as is.
        let srs = dir.path().join("hermez-raw-19");
        std::fs::write(&srs, "srs").unwrap();
        subprocess_prover_config(Some(step), None, None, Some(srs), false, 1, logs).unwrap();
    }
}
