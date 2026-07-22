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

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy::{
    network::{EthereumWallet, Network},
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    signers::{local::PrivateKeySigner, Signer},
};
use bridge_relayer_daemon::{
    discover_event_proofs, result_path_for, BackoffConfig, BkSetSentry, BkSetUpdateSubmitOutcome,
    BkUpdateProofsSource, BkUpdateSource, BlockSource, BridgeClient, Circuit4ShplonkPipeline,
    DryRunOutcome, EmptyBkUpdateSource, EthBridgeClient, FixturesBlockSource, GuardedOutcome,
    LiveBlockSource, PartnerWithdrawalProof, ProverProofsBlockSource, Relayer, RelayerConfig,
    RelayerMetrics, SentryGuardedRelayer, SentryStatus, StatePaths, SubprocessAggregator,
    SubprocessAggregatorConfig, SubprocessCircuit4SnarkProver, SubprocessCircuit4SnarkProverConfig,
    SubprocessWithdrawalProver, SubprocessWithdrawalProverConfig, TickOutcome,
    WithdrawSubmitOutcome, WithdrawalProver, WithdrawalResultGate, check_startup_drift,
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
        /// Optional `contracts/ethereum/verifiers` with `*_calldata.bin` for
        /// hybrid R15 deploys. Auto-detected when omitted.
        #[arg(long)]
        verifiers_dir: Option<PathBuf>,
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
        /// Optional `contracts/ethereum/verifiers` with `*_calldata.bin`.
        #[arg(long)]
        verifiers_dir: Option<PathBuf>,
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
        /// Optional `contracts/ethereum/verifiers` with `*_calldata.bin`.
        #[arg(long)]
        verifiers_dir: Option<PathBuf>,
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
    /// Generate one Circuit 4 withdrawal proof from a `PrivateWitness` by
    /// driving the partner `bridge-event-halo2-prover` (in
    /// `crates/an-bridge-prover`). Writes a `proof_event` JSON that
    /// `submit-withdraw` can consume.
    ProveWithdraw {
        /// `PrivateWitness` JSON (from the `bridge-event-witness` builder).
        #[arg(long)]
        witness: PathBuf,
        /// `crates/an-bridge-prover` workspace root (holds
        /// `target/release/bridge-event-halo2-prover`).
        #[arg(long, env = "AN_BRIDGE_PROVER_DIR")]
        an_bridge_prover_dir: PathBuf,
        /// Working dir holding `./params` (SRS + Circuit 4 PK/VK). Defaults to
        /// the prover dir.
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Where to write the resulting `proof_event` JSON.
        #[arg(long, default_value = "./proof_event.json")]
        out: PathBuf,
        /// Seqno stamped into the proof_event.
        #[arg(long, default_value_t = 0)]
        seq_no: u32,
    },
    /// M7 ETH-side path: generate one Circuit 4 withdrawal proof as **SHPLONK
    /// aggregator calldata** the deployed `BridgeWithdrawalAggregatorVerifier`
    /// accepts. Re-proves the `PrivateWitness` with a Poseidon transcript
    /// (`export-c4-poseidon-snark --fixture`), aggregates the inner snark
    /// (`aggregate-proof`, which self-checks the regenerated Yul == committed
    /// `.bin`), cross-checks the calldata binds the ten public inputs, and
    /// writes a `proof_event` JSON that `submit-withdraw` / `daemon-withdraw`
    /// consume unchanged.
    ProveWithdrawShplonk {
        /// `PrivateWitness` JSON (from the `bridge-event-witness` builder).
        #[arg(long)]
        witness: PathBuf,
        /// `crates/bridge-prover-orchestrator` root (holds
        /// `target/release/export-c4-poseidon-snark`).
        #[arg(long, env = "ORCHESTRATOR_DIR")]
        orchestrator_dir: PathBuf,
        /// `crates/bridge-evm-aggregator` root (holds
        /// `target/release/aggregate-proof`).
        #[arg(long, env = "AGGREGATOR_DIR")]
        aggregator_dir: PathBuf,
        /// Directory of committed verifier `.bin` files (the aggregator's
        /// byte-identity self-check target).
        #[arg(long, default_value = "../../contracts/ethereum/verifiers")]
        verifiers_dir: PathBuf,
        /// Directory holding `kzg_bn254_*.srs` + Circuit-4 keys.
        #[arg(long, default_value = "../../params")]
        params_dir: PathBuf,
        /// Scratch dir for the intermediate `circuit4.snark` /
        /// `.instances.bin`.
        #[arg(long, default_value = "./shplonk-snark")]
        snark_dir: PathBuf,
        /// Where to write the resulting `proof_event` JSON.
        #[arg(long, default_value = "./proof_event.json")]
        out: PathBuf,
        /// Seqno stamped into the proof_event.
        #[arg(long, default_value_t = 0)]
        seq_no: u64,
    },
    /// Long-running daemon reading partner `proof_event_*.json` bundles from
    /// `bridge-verifier-daemon` and submitting `withdrawByProof` on Ethereum.
    /// The withdraw-side twin of `daemon-prover`: it gates on the sibling
    /// `proof_event_*.result.json` ACK, skips nullifiers already consumed
    /// on-chain (idempotent restart), and retries transient reverts with
    /// exponential backoff until SIGINT/SIGTERM.
    DaemonWithdraw {
        /// Directory containing `proof_event_*.json` + `*.result.json`
        /// (partner verifier daemon `proofs/` folder).
        #[arg(long, env = "PROVER_PROOFS_DIR")]
        proofs_dir: PathBuf,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        /// Seconds to sleep between directory scans when idle.
        #[arg(long, default_value_t = 15)]
        poll_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 60)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
        /// Skip the partner `proof_event_*.result.json` verification gate.
        #[arg(long)]
        skip_verified_gate: bool,
        /// Simulate (`eth_call`) each pending withdrawal but never send a
        /// transaction. Useful to confirm the pipeline before spending gas.
        #[arg(long)]
        dry_run: bool,
    },
    /// Unified AN→ETH daemon: **one process, one relayer EOA** that runs
    /// BOTH withdrawal-path legs by interleaving them in a single loop —
    /// no two-service split, no concurrent transactions on the same key.
    ///
    /// Each iteration does, in order:
    ///   1. one `daemon-prover` tick — advance the on-chain AN anchor from the
    ///      next available partner `proof_<seqno>.json` (`verifyBlock`);
    ///   2. one `daemon-withdraw` scan — pay out every ready
    ///      `proof_event_*.json` (`withdrawByProof`), gated on the sibling
    ///      `*.result.json` ACK and skipping nullifiers already used on-chain.
    ///
    /// Both legs read the SAME `--proofs-dir`. Because the two legs run
    /// sequentially in one task sharing one provider, there is only ever a
    /// single in-flight transaction, so nonces never race. Shared
    /// exponential backoff (interruptible by SIGINT/SIGTERM); the prover
    /// cursor persists to `--state`.
    DaemonBridge {
        /// Directory containing BOTH `proof_<seqno>.json` (+ `result_*.json`)
        /// and `proof_event_*.json` (+ `*.result.json`).
        #[arg(long, env = "PROVER_PROOFS_DIR")]
        proofs_dir: PathBuf,
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        /// Seconds to sleep between iterations when idle (no work / all done).
        #[arg(long, default_value_t = 15)]
        poll_secs: u64,
        #[arg(long, default_value_t = 5)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 300)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
        /// Skip the partner `result_*.json` / `proof_event_*.result.json`
        /// verification gate on BOTH legs.
        #[arg(long)]
        skip_verified_gate: bool,
        /// Simulate (`eth_call`) the withdraw leg but never send a
        /// `withdrawByProof` transaction. The prover leg still submits
        /// `verifyBlock` (there is no dry-run for the anchor advance).
        #[arg(long)]
        dry_run: bool,
    },
    /// Live AN→ETH daemon: GraphQL + `LiveProverDriver` → `verifyBlock` /
    /// `applyBkSetUpdate` (Alina live-integration plan).
    ///
    /// Requires SRS/PKs under `--params-dir`, shellnet (or local) GQL, and a
    /// BK-set JSON fallback. Prover state lives under `--prover-state-dir`.
    DaemonLive {
        #[arg(long, env = "RPC_URL")]
        rpc_url: String,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Address,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: String,
        /// Acki Nacki GraphQL endpoint (same as partner `BRIDGE_GQL_ENDPOINT`).
        #[arg(long, env = "BRIDGE_GQL_ENDPOINT")]
        gql_endpoint: String,
        /// Directory with SRS + circuit PKs (`KeyManager`).
        #[arg(long, env = "BRIDGE_PARAMS_DIR", default_value = "./params")]
        params_dir: PathBuf,
        /// Directory for `prover_state.json` / `prover_bk_set.json` /
        /// `bootstrap_seed.json`.
        #[arg(long, env = "BRIDGE_STATE_DIR", default_value = "./state")]
        prover_state_dir: PathBuf,
        /// Fallback BK-set JSON if GQL fetch fails at startup.
        #[arg(
            long,
            env = "BRIDGE_BK_SET_CONFIG",
            default_value = "../an-bridge-prover/bk_set.shellnet.json"
        )]
        bk_set_config: PathBuf,
        /// Optional explicit bootstrap seqno (`SeedPolicy::Explicit`).
        #[arg(long, env = "BRIDGE_BOOTSTRAP_SEQNO")]
        bootstrap_seqno: Option<u64>,
        #[arg(long, default_value_t = 2)]
        backoff_initial_secs: u64,
        #[arg(long, default_value_t = 60)]
        backoff_max_secs: u64,
        #[arg(long, default_value_t = 2)]
        backoff_multiplier: u32,
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
    /// Submit one `applyBkSetUpdate` from partner `bkupd_<seqno>.json`.
    SubmitBkUpdate {
        #[arg(long, env = "PROVER_PROOFS_DIR")]
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
        skip_verified_gate: bool,
        #[arg(long)]
        accept_halo2_proofs: bool,
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
            verifiers_dir,
            rpc_url,
            bridge_address,
            private_key,
            max_ticks,
            an_node_url,
        } => smoke_fixture(
            args.state,
            fixtures_dir,
            verifiers_dir,
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
            verifiers_dir,
            rpc_url,
            bridge_address,
            no_simulate,
        } => verify_fixture(
            fixtures_dir,
            verifiers_dir,
            rpc_url,
            bridge_address,
            !no_simulate,
        )
        .await
        .map_err(|e| {
            error!(?e, "verify-fixture failed");
            e
        }),
        Cmd::Daemon {
            fixtures_dir,
            verifiers_dir,
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
                verifiers_dir,
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
        Cmd::ProveWithdraw {
            witness,
            an_bridge_prover_dir,
            work_dir,
            out,
            seq_no,
        } => prove_withdraw(witness, an_bridge_prover_dir, work_dir, out, seq_no)
            .await
            .map_err(|e| {
                error!(?e, "prove-withdraw failed");
                e
            }),
        Cmd::ProveWithdrawShplonk {
            witness,
            orchestrator_dir,
            aggregator_dir,
            verifiers_dir,
            params_dir,
            snark_dir,
            out,
            seq_no,
        } => prove_withdraw_shplonk(
            witness,
            orchestrator_dir,
            aggregator_dir,
            verifiers_dir,
            params_dir,
            snark_dir,
            out,
            seq_no,
        )
        .await
        .map_err(|e| {
            error!(?e, "prove-withdraw-shplonk failed");
            e
        }),
        Cmd::DaemonWithdraw {
            proofs_dir,
            rpc_url,
            bridge_address,
            private_key,
            poll_secs,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            skip_verified_gate,
            dry_run,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            run_withdraw_daemon(
                proofs_dir,
                rpc_url,
                bridge_address,
                private_key,
                Duration::from_secs(poll_secs),
                backoff,
                skip_verified_gate,
                dry_run,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon-withdraw failed");
                e
            })
        },
        Cmd::DaemonBridge {
            proofs_dir,
            rpc_url,
            bridge_address,
            private_key,
            poll_secs,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
            skip_verified_gate,
            dry_run,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            run_bridge_daemon(
                args.state,
                proofs_dir,
                rpc_url,
                bridge_address,
                private_key,
                Duration::from_secs(poll_secs),
                backoff,
                skip_verified_gate,
                dry_run,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon-bridge failed");
                e
            })
        },
        Cmd::DaemonLive {
            rpc_url,
            bridge_address,
            private_key,
            gql_endpoint,
            params_dir,
            prover_state_dir,
            bk_set_config,
            bootstrap_seqno,
            backoff_initial_secs,
            backoff_max_secs,
            backoff_multiplier,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            run_daemon_live(
                args.state,
                rpc_url,
                bridge_address,
                private_key,
                gql_endpoint,
                params_dir,
                prover_state_dir,
                bk_set_config,
                bootstrap_seqno,
                backoff,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon-live failed");
                e
            })
        },
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
        Cmd::SubmitBkUpdate {
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            skip_verified_gate,
            accept_halo2_proofs,
        } => submit_bk_update(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            skip_verified_gate,
            accept_halo2_proofs,
        )
        .await
        .map_err(|e| {
            error!(?e, "submit-bk-update failed");
            e
        }),
    }
}

#[allow(clippy::too_many_arguments)] // CLI surface; each arg maps to a flag
async fn smoke_fixture(
    state_path: PathBuf,
    fixtures_dir: PathBuf,
    verifiers_dir: Option<PathBuf>,
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
    let source = Arc::new(FixturesBlockSource::open(
        &fixtures_dir,
        verifiers_dir.as_deref(),
    )?);

    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge)?;

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
    verifiers_dir: Option<PathBuf>,
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
    let source = Arc::new(FixturesBlockSource::open(
        &fixtures_dir,
        verifiers_dir.as_deref(),
    )?);
    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge)?;
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
    verifiers_dir: Option<PathBuf>,
    rpc_url: String,
    bridge_address: Address,
    simulate: bool,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);
    let source = FixturesBlockSource::open(&fixtures_dir, verifiers_dir.as_deref())?;

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
    let mut relayer = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), bridge)?;
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

async fn prove_withdraw(
    witness: PathBuf,
    an_bridge_prover_dir: PathBuf,
    work_dir: Option<PathBuf>,
    out: PathBuf,
    seq_no: u32,
) -> anyhow::Result<()> {
    let mut cfg = SubprocessWithdrawalProverConfig::new(an_bridge_prover_dir);
    if let Some(wd) = work_dir {
        cfg.work_dir = wd;
    }
    cfg.seq_no = seq_no;
    let prover = SubprocessWithdrawalProver::new(cfg);

    info!(witness = %witness.display(), "generating Circuit 4 withdrawal proof (this may take minutes)");
    let proof = prover.prove(&witness).await?;

    // Persist in the `proof_event` schema that `submit-withdraw` reads.
    let json = serde_json::json!({
        "schema_version": proof.schema_version,
        "seq_no": proof.seq_no,
        "proof_hex": proof.proof_hex,
        "public_instances_hex": proof.public_instances_hex,
        "self_verified": proof.self_verified,
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&json)?)?;
    info!(
        out = %out.display(),
        public_inputs = proof.public_instances_hex.len(),
        self_verified = proof.self_verified,
        "withdrawal proof written; submit with `relayer submit-withdraw --proof-event {}`",
        out.display()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn prove_withdraw_shplonk(
    witness: PathBuf,
    orchestrator_dir: PathBuf,
    aggregator_dir: PathBuf,
    verifiers_dir: PathBuf,
    params_dir: PathBuf,
    snark_dir: PathBuf,
    out: PathBuf,
    seq_no: u64,
) -> anyhow::Result<()> {
    let snark_prover = SubprocessCircuit4SnarkProver::new(
        SubprocessCircuit4SnarkProverConfig::new(&orchestrator_dir, &params_dir),
    );
    let aggregator = SubprocessAggregator::new(SubprocessAggregatorConfig::new(
        &aggregator_dir,
        &verifiers_dir,
        &params_dir,
    ));
    let pipeline = Circuit4ShplonkPipeline::new(snark_prover, aggregator);

    info!(
        witness = %witness.display(),
        "M7: re-proving Circuit 4 (Poseidon) → aggregating → calldata (this may take minutes)"
    );
    let proof = pipeline.prove(&witness, &snark_dir, seq_no).await?;

    // Persist in the `proof_event` schema that `submit-withdraw` reads. The
    // `proof_hex` here is the SHPLONK aggregator calldata (not raw Halo2).
    let json = serde_json::json!({
        "schema_version": proof.schema_version,
        "seq_no": proof.seq_no,
        "proof_hex": proof.proof_hex,
        "public_instances_hex": proof.public_instances_hex,
        "self_verified": proof.self_verified,
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&json)?)?;
    let calldata_len = proof.proof_bytes().map(|b| b.len()).unwrap_or(0);
    info!(
        out = %out.display(),
        calldata_bytes = calldata_len,
        public_inputs = proof.public_instances_hex.len(),
        "SHPLONK withdrawal calldata written; submit with `relayer submit-withdraw --proof-event {}`",
        out.display()
    );
    Ok(())
}

/// Cross-scan bookkeeping for the withdraw leg. `done` holds proofs fully
/// handled this process lifetime (paid, already-used, or permanently
/// rejected) so re-scanning the directory is cheap; on-chain
/// `isNullifierUsed` is the durable idempotency source across restarts.
#[derive(Default)]
struct WithdrawScanState {
    done: HashSet<PathBuf>,
    paid: u64,
    skipped: u64,
}

/// SIGINT (+ SIGTERM on Unix) shutdown future shared by the long-running
/// daemons. Resolves on the first signal; systemd sends SIGTERM by default.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => info!("SIGINT received"),
                    _ = sigterm.recv() => info!("SIGTERM received"),
                }
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

/// Run **one full scan** of `proofs_dir` for ready `proof_event_*.json`
/// bundles, submitting `withdrawByProof` for each. Gates each proof on its
/// `*.result.json` ACK (unless `skip_verified_gate`), skips nullifiers
/// already consumed on-chain, and (when `dry_run`) only `eth_call`-simulates.
///
/// Returns `Ok(true)` when a *transient* failure occurred (a read/dry-run/
/// submit revert), signalling the caller to back off before the next scan.
/// Permanent per-proof problems (parse errors, bad public inputs, non-256-B
/// proofs, or an ACK that isn't accepted) park the proof in `st.done` and
/// the scan drains the rest of the directory. Lower-level infra failures
/// (RPC down during dry-run/submit) propagate as `Err`.
async fn withdraw_scan_once<P, N>(
    bridge: &EthBridgeClient<P, N>,
    proofs_dir: &Path,
    skip_verified_gate: bool,
    dry_run: bool,
    st: &mut WithdrawScanState,
) -> anyhow::Result<bool>
where
    P: Provider<N> + Clone,
    N: Network,
{
    let mut had_transient_failure = false;
    let proofs = match discover_event_proofs(proofs_dir) {
        Ok(p) => p,
        Err(e) => {
            warn!(?e, "discovery failed; will retry");
            Vec::new()
        },
    };

    for proof_path in proofs {
        if st.done.contains(&proof_path) {
            continue;
        }

        // Gate on the verifier ACK unless explicitly skipped.
        if !skip_verified_gate {
            let result_path = result_path_for(&proof_path);
            match std::fs::read(&result_path) {
                Ok(bytes) => match WithdrawalResultGate::from_json_bytes(&bytes) {
                    Ok(gate) if gate.is_accepted() => {},
                    Ok(_) => {
                        warn!(proof = %proof_path.display(), "verifier ACK present but not accepted (verified/anchor_matched/proof_valid); parking");
                        st.done.insert(proof_path);
                        continue;
                    },
                    Err(e) => {
                        warn!(?e, proof = %proof_path.display(), "unpardeable result ACK; skipping this scan");
                        continue;
                    },
                },
                Err(_) => {
                    // ACK not written yet — verifier hasn't finished.
                    info!(proof = %proof_path.display(), "no result ACK yet; will re-check");
                    continue;
                },
            }
        }

        let bundle = match std::fs::read(&proof_path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", proof_path.display()))
            .and_then(|b| Ok(PartnerWithdrawalProof::from_json_bytes(&b)?))
        {
            Ok(b) => b,
            Err(e) => {
                warn!(?e, proof = %proof_path.display(), "parse failed; parking");
                st.done.insert(proof_path);
                continue;
            },
        };

        let pub_inputs = match bundle.public_inputs() {
            Ok(pi) => pi,
            Err(e) => {
                warn!(?e, proof = %proof_path.display(), "bad public inputs; parking");
                st.done.insert(proof_path);
                continue;
            },
        };

        // Idempotency: skip anything already withdrawn on-chain.
        match bridge.is_nullifier_used(pub_inputs.nullifier).await {
            Ok(true) => {
                info!(proof = %proof_path.display(), nullifier = %pub_inputs.nullifier, "nullifier already used on-chain; skipping");
                st.skipped += 1;
                st.done.insert(proof_path);
                continue;
            },
            Ok(false) => {},
            Err(e) => {
                warn!(?e, "isNullifierUsed read failed; backing off");
                had_transient_failure = true;
                break;
            },
        }

        let proof_bytes = match bundle.proof_bytes() {
            Ok(b) => b,
            Err(e) => {
                warn!(?e, proof = %proof_path.display(), "proof not 256-B Groth16 (gnark-wrap first); parking");
                st.done.insert(proof_path);
                continue;
            },
        };

        if dry_run {
            match bridge.dry_run_withdraw(&proof_bytes, &pub_inputs).await? {
                DryRunOutcome::WouldSucceed => {
                    info!(proof = %proof_path.display(), "dry-run: withdrawByProof would succeed");
                    st.done.insert(proof_path);
                },
                DryRunOutcome::WouldRevert {
                    reason,
                } => {
                    warn!(proof = %proof_path.display(), %reason, "dry-run reverted; will retry (anchor may not be registered yet)");
                    had_transient_failure = true;
                    break;
                },
            }
            continue;
        }

        match bridge.submit_withdraw(&proof_bytes, &pub_inputs).await? {
            WithdrawSubmitOutcome::Paid {
                tx_hash,
            } => {
                info!(proof = %proof_path.display(), ?tx_hash, "withdrawByProof PAID");
                st.paid += 1;
                st.done.insert(proof_path);
            },
            WithdrawSubmitOutcome::Reverted {
                reason,
            } => {
                warn!(proof = %proof_path.display(), %reason, "withdrawByProof reverted; will retry with backoff");
                had_transient_failure = true;
                break;
            },
        }
    }

    Ok(had_transient_failure)
}

/// Withdraw-side twin of `run_prover_daemon`. Polls `proofs_dir` for
/// `proof_event_*.json` bundles, gates each on its `*.result.json` ACK,
/// skips nullifiers already consumed on-chain (so restarts are idempotent
/// and re-scanning the same directory is cheap), and submits
/// `withdrawByProof`. Transient reverts (e.g. anchor not yet registered by
/// the verifyBlock lane) back off exponentially and are retried; permanent
/// per-proof failures are logged and the proof is parked so the loop keeps
/// draining the rest of the directory.
#[allow(clippy::too_many_arguments)]
async fn run_withdraw_daemon(
    proofs_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    poll_interval: Duration,
    backoff: BackoffConfig,
    skip_verified_gate: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(
        proofs_dir = %proofs_dir.display(),
        ?poll_interval,
        dry_run,
        skip_verified_gate,
        "daemon-withdraw starting"
    );

    let mut st = WithdrawScanState::default();
    let mut current_backoff = backoff.initial;

    loop {
        let had_transient_failure =
            withdraw_scan_once(&bridge, &proofs_dir, skip_verified_gate, dry_run, &mut st).await?;

        info!(
            paid = st.paid,
            skipped = st.skipped,
            "daemon-withdraw scan complete"
        );

        // Sleep: backoff after a transient failure, otherwise the idle poll
        // interval. Either sleep is interruptible by shutdown.
        let sleep_for = if had_transient_failure {
            let d = current_backoff;
            current_backoff = std::cmp::min(current_backoff * backoff.multiplier, backoff.max);
            d
        } else {
            current_backoff = backoff.initial;
            poll_interval
        };

        tokio::select! {
            _ = &mut shutdown => {
                info!(paid = st.paid, skipped = st.skipped, "daemon-withdraw stopped");
                return Ok(());
            },
            _ = tokio::time::sleep(sleep_for) => {},
        }
    }
}

/// Unified AN→ETH daemon (the `daemon-bridge` subcommand). Interleaves the
/// two withdrawal-path legs in a single loop on a single relayer EOA:
///
///   1. one prover tick — `Relayer::tick()` advances the on-chain anchor from
///      the next available `proof_<seqno>.json` (`verifyBlock`);
///   2. one withdraw scan — `withdraw_scan_once` pays out every ready
///      `proof_event_*.json` (`withdrawByProof`).
///
/// Running them sequentially in one task, sharing one provider, means there
/// is never more than one in-flight transaction, so the shared EOA's nonces
/// can't race (the failure mode a two-service split invites). A transient
/// failure in *either* leg trips the shared exponential backoff; a clean
/// idle iteration resets it to the poll interval. Shutdown (SIGINT/SIGTERM)
/// is honoured at the sleep boundary, so a `Verified` tick always flushes
/// `state.json` before exit.
#[allow(clippy::too_many_arguments)]
async fn run_bridge_daemon(
    state_path: PathBuf,
    proofs_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    poll_interval: Duration,
    backoff: BackoffConfig,
    skip_verified_gate: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);

    // Prover leg: cursor-driven `Relayer` over the partner proof bundles.
    let source =
        Arc::new(ProverProofsBlockSource::new(&proofs_dir).skip_verified_gate(skip_verified_gate));
    let prover_bridge = Arc::new(EthBridgeClient::new(bridge_address, provider.clone()));
    let cfg = RelayerConfig::new(state_path);
    let mut relayer = Relayer::new(cfg, source, Arc::new(EmptyBkUpdateSource), prover_bridge)?;

    // Withdraw leg: shares the SAME provider (one nonce source; the two
    // legs run sequentially so there's only ever one in-flight tx).
    let wd_bridge = EthBridgeClient::new(bridge_address, provider);
    let mut wd_state = WithdrawScanState::default();

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(
        proofs_dir = %proofs_dir.display(),
        ?poll_interval,
        dry_run,
        skip_verified_gate,
        "daemon-bridge starting (unified verifyBlock + withdrawByProof)"
    );

    let mut current_backoff = backoff.initial;

    loop {
        let mut had_transient = false;

        // ── Leg 1: advance the on-chain anchor (verifyBlock). ──────────
        match relayer.tick().await {
            Ok(TickOutcome::Verified {
                seq_no, ..
            }) => info!(
                seq_no,
                "daemon-bridge: verifyBlock verified (anchor advanced)"
            ),
            Ok(TickOutcome::BkUpdateApplied {
                seq_no, ..
            }) => info!(
                seq_no,
                "daemon-bridge: applyBkSetUpdate applied"
            ),
            Ok(TickOutcome::NotYetAvailable {
                ..
            }) => {},
            Ok(TickOutcome::BridgeReverted {
                target_seq_no,
                reason,
            }) => {
                warn!(target_seq_no, %reason, "daemon-bridge: verifyBlock reverted; backing off");
                had_transient = true;
            },
            Ok(TickOutcome::BkUpdateReverted {
                seq_no,
                reason,
            }) => {
                warn!(seq_no, %reason, "daemon-bridge: applyBkSetUpdate reverted; backing off");
                had_transient = true;
            },
            Err(e) => {
                warn!(?e, "daemon-bridge: verifyBlock tick failed; backing off");
                had_transient = true;
            },
        }

        // ── Leg 2: pay out ready withdrawal proofs (withdrawByProof). ──
        match withdraw_scan_once(
            &wd_bridge,
            &proofs_dir,
            skip_verified_gate,
            dry_run,
            &mut wd_state,
        )
        .await
        {
            Ok(transient) => had_transient |= transient,
            Err(e) => {
                warn!(?e, "daemon-bridge: withdraw scan hard error; backing off");
                had_transient = true;
            },
        }
        info!(
            paid = wd_state.paid,
            skipped = wd_state.skipped,
            "daemon-bridge: withdraw scan complete"
        );

        // ── Shared backoff / poll sleep, interruptible by shutdown. ────
        let sleep_for = if had_transient {
            let d = current_backoff;
            current_backoff = std::cmp::min(current_backoff * backoff.multiplier, backoff.max);
            d
        } else {
            current_backoff = backoff.initial;
            poll_interval
        };

        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!(
                    paid = wd_state.paid,
                    skipped = wd_state.skipped,
                    "daemon-bridge stopped"
                );
                return Ok(());
            },
            _ = tokio::time::sleep(sleep_for) => {},
        }
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

async fn submit_bk_update(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    skip_verified_gate: bool,
    accept_halo2_proofs: bool,
) -> anyhow::Result<()> {
    let source = BkUpdateProofsSource::new(&proofs_dir)
        .skip_verified_gate(skip_verified_gate)
        .accept_halo2_proofs(accept_halo2_proofs);
    let update = source
        .fetch_bk_update(block_seq_no)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no bkupd bundle for seq_no={block_seq_no}"))?;

    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);
    let bridge = EthBridgeClient::new(bridge_address, provider);

    match bridge.submit_bk_set_update(&update).await? {
        BkSetUpdateSubmitOutcome::Applied {
            tx_hash,
            ..
        } => info!(?tx_hash, seq_no = block_seq_no, "applyBkSetUpdate applied"),
        BkSetUpdateSubmitOutcome::Reverted {
            reason,
        } => anyhow::bail!("applyBkSetUpdate reverted: {reason}"),
    }
    Ok(())
}

/// Live GraphQL + `LiveProverDriver` → ETH `verifyBlock` / `applyBkSetUpdate`.
#[allow(clippy::too_many_arguments)]
async fn run_daemon_live(
    state_path: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    gql_endpoint: String,
    params_dir: PathBuf,
    prover_state_dir: PathBuf,
    bk_set_config: PathBuf,
    bootstrap_seqno: Option<u64>,
    backoff: BackoffConfig,
) -> anyhow::Result<()> {
    use bridge_prover_lib::{
        bk_set_fetcher::load_bk_set_from_config,
        bridge_state::BridgeState,
        gql_client::create_client,
        keys::KeyManager,
        live_driver::{LiveProverConfig, LiveProverDriver, SeedPolicy, HISTORY_WINDOW_SIZE},
        prover_bk_set::ProverBkSet,
    };
    use tokio::sync::Mutex;

    std::fs::create_dir_all(&prover_state_dir)?;
    let state_paths = StatePaths::under(&prover_state_dir);
    let prover_state_path = state_paths.prover_state_json.clone();
    let prover_bk_set_path = state_paths.prover_bk_set_json.clone();

    let gql = create_client(&gql_endpoint)
        .map_err(|e| anyhow::anyhow!("create GQL client: {e}"))?;

    // Load genesis BK set from JSON config. The old GraphQL-first path
    // (`fetch_bk_set`) was disabled on 2026-07-22 as architecturally broken:
    // it replayed the `bkSetUpdates` delta log from ∅ but AN does not emit
    // genesis as a synthetic `Added` event. Distant-block cold starts on
    // long-lived rotating chains should use `bk_set_at_height` (planned).
    let bk_set = {
        let path = bk_set_config
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("bk_set_config path not UTF-8"))?;
        let s = load_bk_set_from_config(path)
            .map_err(|e| anyhow::anyhow!("load BK set from {path}: {e}"))?;
        info!(signers = s.len(), path = %path, "BK set loaded from config");
        s
    };

    info!(params_dir = %params_dir.display(), "loading KeyManager (ensure keys)");
    let mut key_manager = KeyManager::new(&params_dir);
    key_manager
        .ensure_primary_keys(&bk_set)
        .map_err(|e| anyhow::anyhow!("ensure_primary_keys: {e}"))?;
    key_manager
        .ensure_fallback_keys(&bk_set)
        .map_err(|e| anyhow::anyhow!("ensure_fallback_keys: {e}"))?;
    key_manager
        .ensure_layer_keys()
        .map_err(|e| anyhow::anyhow!("ensure_layer_keys: {e}"))?;

    let state_path_str = prover_state_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("prover_state path not UTF-8"))?;
    let state = BridgeState::load(state_path_str, HISTORY_WINDOW_SIZE as usize)
        .map_err(|e| anyhow::anyhow!("load BridgeState: {e}"))?;

    let bk_path_str = prover_bk_set_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("prover_bk_set path not UTF-8"))?;
    let prover_bk_set = match ProverBkSet::load(bk_path_str)
        .map_err(|e| anyhow::anyhow!("load ProverBkSet: {e}"))?
    {
        Some(loaded) => {
            if state.initialized && loaded.commitment != state.stored_bk_set_commitment {
                anyhow::bail!(
                    "prover_bk_set commitment {} disagrees with prover_state {} — \
                     delete BOTH under {} or restore a paired backup",
                    hex::encode(loaded.commitment),
                    hex::encode(state.stored_bk_set_commitment),
                    prover_state_dir.display(),
                );
            }
            loaded
        }
        None => {
            let pbs = ProverBkSet::from_pubkeys(&bk_set, 0);
            pbs.save(bk_path_str)
                .map_err(|e| anyhow::anyhow!("save ProverBkSet: {e}"))?;
            info!(
                signers = pbs.pubkeys_hex.len(),
                "bootstrapped prover_bk_set.json"
            );
            pbs
        }
    };

    // Prefer the persisted pubkey table once it exists.
    let bk_set = prover_bk_set
        .pubkeys()
        .map_err(|e| anyhow::anyhow!("prover_bk_set.pubkeys: {e}"))?;

    let seed_policy = match (bootstrap_seqno, state.initialized) {
        (_, true) => SeedPolicy::Resume,
        (Some(n), false) => SeedPolicy::Explicit(n),
        (None, false) => SeedPolicy::Auto,
    };
    info!(?seed_policy, "LiveProverDriver seed policy");

    let driver = LiveProverDriver::new(
        gql,
        key_manager,
        state,
        prover_bk_set,
        bk_set,
        LiveProverConfig {
            seed_policy,
            ..Default::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("LiveProverDriver::new: {e}"))?;
    let driver = Arc::new(Mutex::new(driver));

    let live_source = Arc::new(LiveBlockSource::new(Arc::clone(&driver), state_paths));

    let signer: PrivateKeySigner = private_key.parse()?;
    let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
    let chain_id = probe.get_chain_id().await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);
    let bridge = Arc::new(EthBridgeClient::new(bridge_address, provider));

    // Startup drift audit (§5.4.5).
    let on_chain = bridge.read_state().await?;
    let driver_state = live_source.driver_snapshot().await;
    info!(
        on_chain_last_seen = on_chain.last_seen_block_seq_no,
        driver_last_seen = driver_state.stored_last_seen_block_seq_no,
        on_chain_bk_upd = on_chain.last_bk_set_update_seq_no,
        driver_bk_upd = driver_state.stored_last_bk_set_update_seq_no,
        "daemon-live startup anchors"
    );
    if driver_state.initialized
        && driver_state.stored_last_seen_block_seq_no != on_chain.last_seen_block_seq_no
        && on_chain.last_seen_block_seq_no != 0
        && driver_state.stored_last_seen_block_seq_no != 0
    {
        anyhow::bail!(
            "startup drift: driver last_seen={} vs on-chain {} — nuke {} and rebootstrap \
             (do NOT auto-heal)",
            driver_state.stored_last_seen_block_seq_no,
            on_chain.last_seen_block_seq_no,
            prover_state_dir.display(),
        );
    }

    let cfg = RelayerConfig::new(&state_path);
    let mut relayer = Relayer::new(
        cfg,
        Arc::clone(&live_source),
        Arc::clone(&live_source),
        bridge,
    )?;

    if let Some(remembered) = relayer.state().last_observed_on_chain.clone() {
        let actual = relayer.bridge().read_state().await?;
        check_startup_drift(&remembered, &actual).map_err(|d| {
            anyhow::anyhow!(
                "startup on-chain drift vs last_observed_on_chain: {d} — operator must reconcile"
            )
        })?;
    }

    let metrics = RelayerMetrics::new();
    let shutdown = shutdown_signal();
    info!(
        gql = %gql_endpoint,
        params = %params_dir.display(),
        prover_state = %prover_state_dir.display(),
        "daemon-live starting"
    );
    let summary = relayer
        .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
        .await?;
    info!(?summary, snapshot = ?metrics.snapshot(), "daemon-live stopped");
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
