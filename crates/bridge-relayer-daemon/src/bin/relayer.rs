//! Relayer CLI binary.
//!
//! Core subcommands:
//!
//! - `smoke-fixture` — submits one canned block from a Phase 4.1 bound-proof
//!   fixture directory through the real
//!   [`bridge_relayer_daemon::EthBridgeClient`].
//!
//! - `daemon` — long-running operator entry point. Drives `Relayer::tick`
//!   forever with exponential backoff, until SIGINT/SIGTERM. Logs a structured
//!   metrics snapshot on every shutdown.
//!
//! - `verify-fixture` — **read-only** pre-flight check. Loads a fixture, reads
//!   the on-chain bridge anchors over RPC, and reports field-by-field whether
//!   the fixture would be accepted by `verifyBlock` (the cheap pre-crypto
//!   checks: bk-set commitment match, monotonic seqNo, prev- anchor match). No
//!   private key, no submission. Exits non-zero on any mismatch so it slots
//!   into a pre-deploy shell pipeline.
//!
//! BK-set rotations are handled at the source seam by
//! `bridge_prover_lib::live_driver::LiveProverDriver` (used by the
//! `daemon-live` subcommand). The old REST-based `BkSetSentry` /
//! `SentryGuardedRelayer` / `sentry-watch` surface was retired 2026-07-30:
//! port 8600 REST is internal-only on public shellnet since AN v0.16.3.

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
use bridge_event_witness::AnchorLayerMode;
use bridge_relayer_daemon::{
    check_startup_drift, discover_event_proofs, run_withdraw_e2e_once, BackoffConfig,
    BkSetUpdateSubmitOutcome, BkUpdateProofsSource, BkUpdateSource, BlockSource, BridgeClient,
    Circuit4ShplonkPipeline, DryRunOutcome, EmptyBkUpdateSource, EthBridgeClient,
    FixturesBlockSource, InProcessCircuit4SnarkProver, LiveBlockSource, PartnerWithdrawalProof,
    ProverProofsBlockSource, Relayer, RelayerConfig, RelayerMetrics, StatePaths,
    SubprocessAggregator, SubprocessAggregatorConfig, SubprocessWithdrawalProver,
    SubprocessWithdrawalProverConfig, TickOutcome, WithdrawE2EConfig, WithdrawSubmitOutcome,
    WithdrawalProver,
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
        /// Allow ~8 KB Halo2 proof bytes through load (anchor checks only).
        #[arg(long)]
        accept_halo2_proofs: bool,
    },
    /// Generate one Circuit 4 withdrawal proof from a `PrivateWitness` by
    /// driving the partner `bridge-event-halo2-prover` (in
    /// `crates/bridge-prover-libraries`). Writes a `proof_event` JSON that
    /// `submit-withdraw` can consume.
    ProveWithdraw {
        /// `PrivateWitness` JSON (from the `bridge-event-witness` builder).
        #[arg(long)]
        witness: PathBuf,
        /// `crates/bridge-prover-libraries` workspace root (holds
        /// `target/release/bridge-event-halo2-prover`).
        #[arg(long, env = "BRIDGE_PROVER_LIBRARIES_DIR")]
        bridge_prover_libraries_dir: PathBuf,
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
    /// accepts. Re-proves the `PrivateWitness` in-process with a Poseidon
    /// transcript ([`InProcessCircuit4SnarkProver`], NB-Q9 PR-B; supersedes
    /// the historical `export-c4-poseidon-snark --fixture` subprocess),
    /// aggregates the inner snark (`aggregate-proof`, which self-checks the
    /// regenerated Yul == committed `.bin`), cross-checks the calldata binds
    /// the ten public inputs, and writes a `proof_event` JSON that
    /// `submit-withdraw` / `daemon-withdraw` consume unchanged.
    ProveWithdrawShplonk {
        /// `PrivateWitness` JSON (from the `bridge-event-witness` builder).
        #[arg(long)]
        witness: PathBuf,
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
        /// Persistent outer-PK cache for the `aggregate-proof` subprocess.
        /// Defaults to `<params_dir>/pk_cache`. See `daemon-live
        /// --pk-cache-dir`.
        #[arg(long, env = "BRIDGE_PK_CACHE_DIR")]
        pk_cache_dir: Option<PathBuf>,
    },
    /// Long-running daemon reading `proof_event_*.json` bundles and submitting
    /// `withdrawByProof` on Ethereum. The withdraw-side twin of
    /// `daemon-prover`: skips nullifiers already consumed on-chain (idempotent
    /// restart) and retries transient reverts with exponential backoff until
    /// SIGINT/SIGTERM.
    DaemonWithdraw {
        /// Directory containing `proof_event_*.json` bundles.
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
    ///      `proof_event_*.json` (`withdrawByProof`), skipping nullifiers
    ///      already used on-chain.
    ///
    /// Both legs read the SAME `--proofs-dir`. Because the two legs run
    /// sequentially in one task sharing one provider, there is only ever a
    /// single in-flight transaction, so nonces never race. Shared
    /// exponential backoff (interruptible by SIGINT/SIGTERM); the prover
    /// cursor persists to `--state`.
    DaemonBridge {
        /// Directory containing BOTH `proof_<seqno>.json` and
        /// `proof_event_*.json` bundles.
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
        /// Comma-separated failover GraphQL endpoints, tried in order after
        /// `--gql-endpoint` exhausts its retries. Every request starts from
        /// `--gql-endpoint` and cycles through the whole list until one
        /// attempt succeeds. Retry policy: `BRIDGE_GQL_RETRIES_PER_ENDPOINT`
        /// (3), `BRIDGE_GQL_RETRY_DELAY_MS` (1000),
        /// `BRIDGE_GQL_REQUEST_TIMEOUT_SECS` (30),
        /// `BRIDGE_GQL_CONNECT_TIMEOUT_SECS` (10), `BRIDGE_GQL_MAX_ROUNDS`
        /// (unset = loop forever).
        #[arg(long, env = "BRIDGE_GQL_FAILOVER_ENDPOINTS")]
        gql_failover_endpoints: Option<String>,
        /// Bind address for the Prometheus text exporter (`GET /metrics`),
        /// e.g. `0.0.0.0:9464`. Unset = no exporter.
        #[arg(long, env = "RELAYER_METRICS_ADDR")]
        metrics_addr: Option<std::net::SocketAddr>,
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
            default_value = "../bridge-prover-libraries/bk_set.shellnet.json"
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
        /// Path to the `crates/bridge-evm-aggregator` root (used by the
        /// `aggregate-proof` subprocess). C1/C2 R15 SHPLONK aggregation is
        /// mandatory — the on-chain `AckiNackiBridge` verifier only accepts
        /// aggregated calldata, so `daemon-live` refuses to start without it.
        #[arg(long, env = "BRIDGE_AGGREGATOR_DIR")]
        aggregator_dir: PathBuf,
        /// Directory of committed verifier `.bin` files (aggregator
        /// self-check).
        #[arg(long, env = "BRIDGE_VERIFIERS_DIR")]
        verifiers_dir: PathBuf,
        /// Persistent outer-PK cache directory for the `aggregate-proof`
        /// subprocess (`--pk-cache-dir`). Without this, every bundle re-runs
        /// the full K=21 outer keygen (~3–5 min); with it, only the first
        /// bundle pays keygen and subsequent bundles hit the disk cache
        /// (~15–60 s). Defaults to `<params_dir>/pk_cache`.
        #[arg(long, env = "BRIDGE_PK_CACHE_DIR")]
        pk_cache_dir: Option<PathBuf>,
        /// Anchor level for the LiveProverDriver step math and bundle
        /// stride. `1` = L1 (default, stride W·P = 1024); `2` = L2
        /// (stride W² = 16384). Must match the on-chain genesis stamp
        /// level for the deployed bridge — a mismatch trips the startup
        /// drift check.
        #[arg(long, env = "BRIDGE_ANCHOR_LEVEL", default_value_t = 1)]
        anchor_level: u8,
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
        accept_halo2_proofs: bool,
    },
    /// End-to-end withdraw pipeline: capture live `WithdrawalInitiated`
    /// ExtOut event → export partial witness → enrich → prove → optional
    /// on-chain `withdrawByProof`.
    ///
    /// Replaces the Python driver's steps 5–7 with a single in-process
    /// call (see `bridge_relayer_daemon::withdraw_e2e`). If any of the
    /// three submit-side flags (`--rpc-url`, `--bridge-address`,
    /// `--private-key`) are omitted, the command stops after proving and
    /// only logs the produced proof metadata.
    WithdrawE2E {
        /// GraphQL endpoint (e.g. `https://shellnet.ackinacki.org/bk/v2/graphql`).
        #[arg(long, env = "GQL_ENDPOINT")]
        gql_endpoint: String,
        /// Path to the `prover_state.json` snapshot the enricher reads.
        #[arg(long, env = "PROVER_STATE_PATH")]
        prover_state_path: PathBuf,
        /// History-proof window size (`W`). Must match the value the
        /// state file was written with.
        #[arg(long, default_value_t = 128)]
        window_size: usize,
        /// Emitting bridge account (64-hex, no `0x`).
        #[arg(long)]
        bridge_account_id: String,
        /// Emitting bridge dapp_id (64-hex, no `0x`).
        #[arg(long)]
        bridge_dapp_id: String,
        /// ExtOut `dst` sentinel to filter on. Defaults to
        /// `WithdrawalInitiated`'s `makeAddrExtern(618)` value.
        #[arg(
            long,
            default_value = ":000000000000000000000000000000000000000000000000000000000000026a"
        )]
        event_dst: String,
        /// Total time budget for the event-capture stage, in seconds.
        #[arg(long, default_value_t = 300)]
        event_wait_s: u64,
        /// Poll interval during the capture stage, in seconds.
        #[arg(long, default_value_t = 2)]
        event_poll_interval_s: u64,
        /// Anchor layer mode: `auto` (probe L1 → L2 → …) or an explicit
        /// layer number (`1`, `2`, …).
        #[arg(long, default_value = "auto")]
        anchor_layer: String,
        /// Ack the L(n≥2) wait budget when `--anchor-layer` picks an
        /// explicit `n≥2`. Auto mode counts as an implicit ack.
        #[arg(long)]
        i_know_the_wait: bool,
        /// Where to write the enriched witness JSON.
        #[arg(long)]
        work_dir: PathBuf,
        /// `crates/bridge-evm-aggregator` root (holds
        /// `target/release/aggregate-proof`). Forwarded to
        /// [`SubprocessAggregatorConfig`] inside the C4 SHPLONK pipeline.
        #[arg(long, env = "BRIDGE_AGGREGATOR_DIR")]
        aggregator_dir: PathBuf,
        /// Directory of committed verifier `.bin` files (aggregator's
        /// byte-identity self-check target).
        #[arg(
            long,
            env = "BRIDGE_VERIFIERS_DIR",
            default_value = "../../contracts/ethereum/verifiers"
        )]
        verifiers_dir: PathBuf,
        /// Directory holding `kzg_bn254_*.srs` + Circuit-4 keys.
        #[arg(long, env = "BRIDGE_PARAMS_DIR", default_value = "./params")]
        params_dir: PathBuf,
        /// Scratch dir for the intermediate `circuit4.snark` /
        /// `.instances.bin` from the SHPLONK pipeline.
        #[arg(long, default_value = "./shplonk-snark")]
        snark_dir: PathBuf,
        /// Persistent outer-PK cache for the `aggregate-proof` subprocess.
        /// Defaults to `<params_dir>/pk_cache`.
        #[arg(long, env = "BRIDGE_PK_CACHE_DIR")]
        pk_cache_dir: Option<PathBuf>,
        /// Optional dir to persist `proof_event_{seq:06}.json`.
        #[arg(long)]
        prover_out_dir: Option<PathBuf>,
        /// Aggregator subprocess timeout, in seconds. Circuit-4 outer
        /// keygen from a cold PK cache is a few minutes.
        #[arg(long, default_value_t = 1800)]
        prover_timeout_s: u64,
        /// Seqno stamped into witness/proof filenames.
        #[arg(long, default_value_t = 0)]
        prover_seq_no: u32,
        /// If set together with `--bridge-address` + `--private-key`,
        /// also drive `withdrawByProof` on Ethereum after proving.
        #[arg(long, env = "RPC_URL")]
        rpc_url: Option<String>,
        #[arg(long, env = "BRIDGE_ADDRESS")]
        bridge_address: Option<Address>,
        #[arg(long, env = "RELAYER_PRIVATE_KEY")]
        private_key: Option<String>,
        /// If the submit trio is supplied, do only an `eth_call`
        /// dry-run (no signed tx).
        #[arg(long)]
        dry_run: bool,
        /// Replay mode: skip baseline snapshot, pick the youngest
        /// matching WithdrawalInitiated ExtOut event. Use to recover a
        /// prior burn whose enricher timed out (e.g. daemon crashed
        /// before the covering bundle landed on-chain). On a
        /// single-account demo this unambiguously targets the last burn.
        ///
        /// SAFETY: single-account / operator-controlled recipient EOA
        /// only. On any shared relayer wallet this would let one
        /// operator prove another operator's `WithdrawalInitiated`, so
        /// this flag is intentionally scoped to `withdraw-e2e` and must
        /// NEVER be wired into `daemon-live` (per PR#35 review round 2).
        #[arg(long)]
        replay_latest: bool,
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
        } => smoke_fixture(
            args.state,
            fixtures_dir,
            verifiers_dir,
            rpc_url,
            bridge_address,
            private_key,
            max_ticks,
        )
        .await
        .map_err(|e| {
            error!(?e, "smoke run failed");
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
                backoff,
            )
            .await
            .map_err(|e| {
                error!(?e, "daemon failed");
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
        } => submit_verify_block(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
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
            accept_halo2_proofs,
        } => verify_prover_proof(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            !no_simulate,
            accept_halo2_proofs,
        )
        .await
        .map_err(|e| {
            error!(?e, "verify-prover-proof failed");
            e
        }),
        Cmd::ProveWithdraw {
            witness,
            bridge_prover_libraries_dir,
            work_dir,
            out,
            seq_no,
        } => prove_withdraw(witness, bridge_prover_libraries_dir, work_dir, out, seq_no)
            .await
            .map_err(|e| {
                error!(?e, "prove-withdraw failed");
                e
            }),
        Cmd::ProveWithdrawShplonk {
            witness,
            aggregator_dir,
            verifiers_dir,
            params_dir,
            snark_dir,
            out,
            seq_no,
            pk_cache_dir,
        } => {
            let pk_cache_dir = pk_cache_dir.unwrap_or_else(|| params_dir.join("pk_cache"));
            prove_withdraw_shplonk(
                witness,
                aggregator_dir,
                verifiers_dir,
                params_dir,
                snark_dir,
                out,
                seq_no,
                pk_cache_dir,
            )
        }
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
            aggregator_dir,
            verifiers_dir,
            pk_cache_dir,
            anchor_level,
            gql_failover_endpoints,
            metrics_addr,
        } => {
            let backoff = BackoffConfig {
                initial: Duration::from_secs(backoff_initial_secs),
                max: Duration::from_secs(backoff_max_secs),
                multiplier: backoff_multiplier,
            };
            // Default the outer-PK cache alongside the SRS so a single
            // params_dir carries every artefact the aggregator needs.
            let pk_cache_dir = pk_cache_dir.unwrap_or_else(|| params_dir.join("pk_cache"));
            let aggregation = C12AggregationCfg {
                aggregator_dir,
                verifiers_dir,
                pk_cache_dir,
            };
            let anchor_mode = bridge_prover_lib::AnchorMode::from_level(anchor_level)
                .map_err(|e| anyhow::anyhow!("--anchor-level: {e}"))?;
            // The exporter must exist before any `metrics::describe_*`
            // call, i.e. before the GraphQL client is built inside
            // `run_daemon_live`.
            if let Some(addr) = metrics_addr {
                install_metrics_exporter(addr)?;
            }
            let gql_failover_endpoints = gql_failover_endpoints
                .as_deref()
                .map(bridge_gql_fetcher::gql_client::parse_endpoint_list)
                .unwrap_or_default();
            run_daemon_live(
                args.state,
                rpc_url,
                bridge_address,
                private_key,
                gql_endpoint,
                gql_failover_endpoints,
                params_dir,
                prover_state_dir,
                bk_set_config,
                bootstrap_seqno,
                backoff,
                aggregation,
                anchor_mode,
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
        Cmd::WithdrawE2E {
            gql_endpoint,
            prover_state_path,
            window_size,
            bridge_account_id,
            bridge_dapp_id,
            event_dst,
            event_wait_s,
            event_poll_interval_s,
            anchor_layer,
            i_know_the_wait,
            work_dir,
            aggregator_dir,
            verifiers_dir,
            params_dir,
            snark_dir,
            pk_cache_dir,
            prover_out_dir,
            prover_timeout_s,
            prover_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
            replay_latest,
        } => withdraw_e2e_cli(WithdrawE2ECliArgs {
            gql_endpoint,
            prover_state_path,
            window_size,
            bridge_account_id,
            bridge_dapp_id,
            event_dst,
            event_wait_s,
            event_poll_interval_s,
            anchor_layer,
            i_know_the_wait,
            work_dir,
            aggregator_dir,
            verifiers_dir,
            params_dir,
            snark_dir,
            pk_cache_dir,
            prover_out_dir,
            prover_timeout_s,
            prover_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            dry_run,
            replay_latest,
        })
        .await
        .map_err(|e| {
            error!(?e, "withdraw-e2e failed");
            e
        }),
        Cmd::SubmitBkUpdate {
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            accept_halo2_proofs,
        } => submit_bk_update(
            proofs_dir,
            block_seq_no,
            rpc_url,
            bridge_address,
            private_key,
            accept_halo2_proofs,
        )
        .await
        .map_err(|e| {
            error!(?e, "submit-bk-update failed");
            e
        }),
    }
}

async fn smoke_fixture(
    state_path: PathBuf,
    fixtures_dir: PathBuf,
    verifiers_dir: Option<PathBuf>,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    max_ticks: usize,
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

    let history = relayer
        .run_loop(max_ticks, |outcome| {
            matches!(outcome, TickOutcome::Verified { .. })
        })
        .await?;
    info!(?history, "smoke run complete");
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

    info!(?backoff, "daemon starting");
    let summary = relayer
        .run_until_shutdown(backoff, Some(metrics.clone()), shutdown)
        .await?;

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

    // Storage v2.0 (2026-08-04): the on-chain
    // `storedPrevMaxLevelLayerHash()` field is now the immutable genesis
    // seed. The runtime anchor lives in `_layerWindows[L]` and is exposed
    // via `expectedPrevAnchor(numLayers)`.
    let chain_anchor = bridge.expected_prev_anchor(block.num_layers).await?;
    if block.prev_max_level_layer_hash != chain_anchor {
        ok = false;
        diagnostics.push(format!(
            "PrevAnchor MISMATCH: fixture = {:#x}, on-chain expectedPrevAnchor({}) = {:#x}",
            block.prev_max_level_layer_hash, block.num_layers, chain_anchor
        ));
    } else {
        info!(
            prev_anchor = ?block.prev_max_level_layer_hash,
            num_layers = block.num_layers,
            "PrevAnchor matches on-chain expectedPrevAnchor",
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

async fn submit_verify_block(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    dry_run: bool,
) -> anyhow::Result<()> {
    let source = ProverProofsBlockSource::new(&proofs_dir);
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

async fn verify_prover_proof(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    simulate: bool,
    accept_halo2_proofs: bool,
) -> anyhow::Result<()> {
    let source = ProverProofsBlockSource::new(&proofs_dir).accept_halo2_proofs(accept_halo2_proofs);
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
    // Storage v2.0 (2026-08-04): compare against per-layer anchor pick.
    let chain_anchor = bridge.expected_prev_anchor(block.num_layers).await?;
    if block.prev_max_level_layer_hash != chain_anchor {
        anyhow::bail!(
            "prev anchor mismatch: proof={} chain_expectedPrevAnchor({})={}",
            block.prev_max_level_layer_hash,
            block.num_layers,
            chain_anchor
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
    bridge_prover_libraries_dir: PathBuf,
    work_dir: Option<PathBuf>,
    out: PathBuf,
    seq_no: u32,
) -> anyhow::Result<()> {
    let mut cfg = SubprocessWithdrawalProverConfig::new(bridge_prover_libraries_dir);
    if let Some(wd) = work_dir {
        cfg.work_dir = wd;
    }
    cfg.seq_no = seq_no;
    let prover = SubprocessWithdrawalProver::new(cfg);

    info!(witness = %witness.display(), "generating Circuit 4 withdrawal proof (this may take minutes)");
    let proof = prover.prove(&witness).await?;

    // Persist in the `proof_event` schema that `submit-withdraw` reads.
    let json = serde_json::json!({
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
    aggregator_dir: PathBuf,
    verifiers_dir: PathBuf,
    params_dir: PathBuf,
    snark_dir: PathBuf,
    out: PathBuf,
    seq_no: u64,
    pk_cache_dir: PathBuf,
) -> anyhow::Result<()> {
    // NB-Q9 PR-B: in-process Circuit 4 Poseidon re-prove (no more subprocess
    // shell-out to `export-c4-poseidon-snark`).
    let snark_prover = InProcessCircuit4SnarkProver::new(&params_dir);
    let aggregator = SubprocessAggregator::new(
        SubprocessAggregatorConfig::new(&aggregator_dir, &verifiers_dir, &params_dir)
            .with_pk_cache_dir(&pk_cache_dir),
    );
    let pipeline = Circuit4ShplonkPipeline::new(snark_prover, aggregator);

    info!(
        witness = %witness.display(),
        "M7: re-proving Circuit 4 (Poseidon, in-process) → aggregating → calldata (this may take minutes)"
    );
    let proof = pipeline.prove(&witness, &snark_dir, seq_no).await?;

    // Persist in the `proof_event` schema that `submit-withdraw` reads. The
    // `proof_hex` here is the SHPLONK aggregator calldata (not raw Halo2).
    let json = serde_json::json!({
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
/// bundles, submitting `withdrawByProof` for each. Skips nullifiers already
/// consumed on-chain, and (when `dry_run`) only `eth_call`-simulates.
///
/// Returns `Ok(true)` when a *transient* failure occurred (a read/dry-run/
/// submit revert), signalling the caller to back off before the next scan.
/// Permanent per-proof problems (parse errors, bad public inputs, proofs
/// that fail the SHPLONK aggregator shape gate) park the proof in `st.done`
/// and the scan drains the rest of the directory. Lower-level infra failures
/// (RPC down during dry-run/submit) propagate as `Err`.
async fn withdraw_scan_once<P, N>(
    bridge: &EthBridgeClient<P, N>,
    proofs_dir: &Path,
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
                warn!(
                    proof = %proof_path.display(),
                    nullifier = %pub_inputs.nullifier,
                    amount = %pub_inputs.amount,
                    recipient_hi = %pub_inputs.recipient_hi,
                    recipient_lo = %pub_inputs.recipient_lo,
                    "nullifier already used on-chain; skipping (benign retry, or BRIDGE-WD-01 same-block duplicate burn — second ECC is stranded until C4 re-keygen)"
                );
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
                warn!(?e, proof = %proof_path.display(), "proof fails SHPLONK aggregator shape gate; parking");
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
/// `proof_event_*.json` bundles, skips nullifiers already consumed on-chain
/// (so restarts are idempotent and re-scanning the same directory is cheap),
/// and submits `withdrawByProof`. Transient reverts (e.g. anchor not yet
/// registered by the verifyBlock lane) back off exponentially and are
/// retried; permanent per-proof failures are logged and the proof is parked
/// so the loop keeps draining the rest of the directory.
async fn run_withdraw_daemon(
    proofs_dir: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    poll_interval: Duration,
    backoff: BackoffConfig,
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
        "daemon-withdraw starting"
    );

    let mut st = WithdrawScanState::default();
    let mut current_backoff = backoff.initial;

    loop {
        let had_transient_failure =
            withdraw_scan_once(&bridge, &proofs_dir, dry_run, &mut st).await?;

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
    let source = Arc::new(ProverProofsBlockSource::new(&proofs_dir));
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
            }) => info!(seq_no, "daemon-bridge: applyBkSetUpdate applied"),
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
        match withdraw_scan_once(&wd_bridge, &proofs_dir, dry_run, &mut wd_state).await {
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

/// Grouped arg struct so the `Cmd::WithdrawE2E` destructure has one
/// name to hand off to [`withdraw_e2e_cli`] instead of 20 positional
/// parameters — avoids the `too_many_arguments` clippy lint that
/// several other subcommands here trip.
struct WithdrawE2ECliArgs {
    gql_endpoint: String,
    prover_state_path: PathBuf,
    window_size: usize,
    bridge_account_id: String,
    bridge_dapp_id: String,
    event_dst: String,
    event_wait_s: u64,
    event_poll_interval_s: u64,
    anchor_layer: String,
    i_know_the_wait: bool,
    work_dir: PathBuf,
    aggregator_dir: PathBuf,
    verifiers_dir: PathBuf,
    params_dir: PathBuf,
    snark_dir: PathBuf,
    pk_cache_dir: Option<PathBuf>,
    prover_out_dir: Option<PathBuf>,
    prover_timeout_s: u64,
    prover_seq_no: u32,
    rpc_url: Option<String>,
    bridge_address: Option<Address>,
    private_key: Option<String>,
    dry_run: bool,
    replay_latest: bool,
}

async fn withdraw_e2e_cli(args: WithdrawE2ECliArgs) -> anyhow::Result<()> {
    let anchor_mode = parse_anchor_layer(&args.anchor_layer)?;

    let cfg = WithdrawE2EConfig {
        gql_endpoint: args.gql_endpoint,
        prover_state_path: args.prover_state_path,
        window_size: args.window_size,
        bridge_account_id_hex: args.bridge_account_id,
        bridge_dapp_id_hex: args.bridge_dapp_id,
        event_dst_filter: args.event_dst,
        event_wait: Duration::from_secs(args.event_wait_s),
        event_poll_interval: Duration::from_secs(args.event_poll_interval_s),
        anchor_mode,
        i_know_the_wait: args.i_know_the_wait,
        work_dir: args.work_dir,
        aggregator_dir: args.aggregator_dir,
        verifiers_dir: args.verifiers_dir,
        params_dir: args.params_dir,
        snark_dir: args.snark_dir,
        pk_cache_dir: args.pk_cache_dir,
        prover_out_dir: args.prover_out_dir,
        prover_timeout: Duration::from_secs(args.prover_timeout_s),
        prover_seq_no: args.prover_seq_no,
        replay_latest: args.replay_latest,
    };

    let summary = run_withdraw_e2e_once(cfg).await?;

    info!(
        message_id = %summary.captured.message_id,
        block_seq_no = summary.captured.block_seq_no,
        block_id = %summary.captured.block_id_hex,
        witness_path = %summary.witness_path.display(),
        proof_len = summary.proof.proof_hex.len() / 2,
        self_verified = summary.proof.self_verified,
        layer_idx = summary.enrich.layer_idx,
        auto_escalated = summary.enrich.auto_escalated,
        "withdraw-e2e: capture + prove complete",
    );

    let submit = match (args.rpc_url, args.bridge_address, args.private_key) {
        (Some(rpc), Some(addr), Some(pk)) => Some((rpc, addr, pk)),
        (None, None, None) => {
            info!("no ETH submit trio supplied — stopping after prove");
            return Ok(());
        },
        _ => anyhow::bail!(
            "must supply all three of --rpc-url / --bridge-address / --private-key together (or \
             none)"
        ),
    };
    let (rpc_url, bridge_address, private_key) = submit.unwrap();

    let proof_bytes = summary.proof.proof_bytes()?;
    let pub_inputs = summary.proof.public_inputs()?;

    if args.dry_run {
        let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let bridge = EthBridgeClient::new(bridge_address, provider);
        match bridge.dry_run_withdraw(&proof_bytes, &pub_inputs).await? {
            DryRunOutcome::WouldSucceed => info!("dry-run: withdrawByProof would succeed"),
            DryRunOutcome::WouldRevert {
                reason,
            } => anyhow::bail!("dry-run reverted: {reason}"),
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

    match bridge.submit_withdraw(&proof_bytes, &pub_inputs).await? {
        WithdrawSubmitOutcome::Paid {
            tx_hash,
        } => info!(?tx_hash, "withdrawByProof paid out"),
        WithdrawSubmitOutcome::Reverted {
            reason,
        } => anyhow::bail!("withdrawByProof reverted: {reason}"),
    }
    Ok(())
}

/// Parse the `--anchor-layer` flag into [`AnchorLayerMode`]. Accepts
/// case-insensitive `"auto"` or a positive layer index.
fn parse_anchor_layer(s: &str) -> anyhow::Result<AnchorLayerMode> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Ok(AnchorLayerMode::Auto);
    }
    let n: u8 = t
        .parse()
        .map_err(|e| anyhow::anyhow!("--anchor-layer must be `auto` or a positive integer: {e}"))?;
    if n == 0 {
        anyhow::bail!("--anchor-layer=0 is not valid; use `1` or higher");
    }
    Ok(AnchorLayerMode::Explicit(n))
}

async fn submit_bk_update(
    proofs_dir: PathBuf,
    block_seq_no: u64,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    accept_halo2_proofs: bool,
) -> anyhow::Result<()> {
    let source = BkUpdateProofsSource::new(&proofs_dir).accept_halo2_proofs(accept_halo2_proofs);
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
            tx_hash, ..
        } => info!(?tx_hash, seq_no = block_seq_no, "applyBkSetUpdate applied"),
        BkSetUpdateSubmitOutcome::Reverted {
            reason,
        } => anyhow::bail!("applyBkSetUpdate reverted: {reason}"),
    }
    Ok(())
}

/// Live GraphQL + `LiveProverDriver` → ETH `verifyBlock` / `applyBkSetUpdate`.
#[allow(clippy::too_many_arguments)]
/// Wiring for the ETH-side Poseidon re-prove + R15 SHPLONK aggregation
/// pipeline. When present, `run_daemon_live` wraps `LiveBlockSource` in an
/// [`AggregatedBlockSource`] so `AnBlockData.attestation_proof` /
/// `AnBlockData.layer_hashes_proof` (and BK-update `attestation_proof`) carry
/// the aggregator calldata the Solidity `AckiNackiBridge` accepts.
struct C12AggregationCfg {
    aggregator_dir: PathBuf,
    verifiers_dir: PathBuf,
    /// Persistent outer-PK cache directory for `aggregate-proof`. When
    /// present, memoises the K=21 outer keygen across bundles.
    pk_cache_dir: PathBuf,
}

// Daemon wiring handler: the args mirror distinct CLI flags, so grouping them
// into a struct would only add indirection.
#[allow(clippy::too_many_arguments)]
async fn run_daemon_live(
    state_path: PathBuf,
    rpc_url: String,
    bridge_address: Address,
    private_key: String,
    gql_endpoint: String,
    gql_failover_endpoints: Vec<String>,
    params_dir: PathBuf,
    prover_state_dir: PathBuf,
    bk_set_config: PathBuf,
    bootstrap_seqno: Option<u64>,
    backoff: BackoffConfig,
    aggregation: C12AggregationCfg,
    anchor_mode: bridge_prover_lib::AnchorMode,
) -> anyhow::Result<()> {
    use bridge_gql_fetcher::gql_client::{create_client_with_failover, GqlClientConfig};
    use bridge_prover_lib::{
        bk_set_bootstrap,
        bridge_state::BridgeState,
        keys::KeyManager,
        live_driver::{LiveProverConfig, LiveProverDriver, SeedPolicy, HISTORY_WINDOW_SIZE},
        prover_bk_set::ProverBkSet,
        transcript::TranscriptKind,
    };
    use tokio::sync::Mutex;

    std::fs::create_dir_all(&prover_state_dir)?;
    let state_paths = StatePaths::under(&prover_state_dir);
    let prover_state_path = state_paths.prover_state_json.clone();
    let prover_bk_set_path = state_paths.prover_bk_set_json.clone();

    // Primary + failover endpoints with the daemon's retry policy: any
    // failed attempt (transport, HTTP status, GraphQL `errors`, missing
    // block) is retried on the same endpoint, then on the next one, cycling
    // until an attempt succeeds. Counted in `relayer_gql_*` metrics.
    let gql_cfg = GqlClientConfig::from_env()
        .map_err(|e| anyhow::anyhow!("GraphQL client configuration: {e}"))?;
    let gql = create_client_with_failover(&gql_endpoint, &gql_failover_endpoints, gql_cfg)
        .map_err(|e| anyhow::anyhow!("create GQL client: {e}"))?;
    info!(
        endpoints = ?gql.endpoints(),
        retries_per_endpoint = gql.config().retries_per_endpoint,
        retry_delay_ms = gql.config().retry_delay.as_millis() as u64,
        request_timeout_secs = gql.config().request_timeout.as_secs(),
        connect_timeout_secs = gql.config().connect_timeout.as_secs(),
        max_rounds = ?gql.config().max_rounds,
        "daemon-live: GraphQL endpoints",
    );

    // Load BK set via the shared bootstrap helper (aligned with
    // `bridge-prover-daemon/src/main.rs`). Mode is selected by
    // `BRIDGE_BK_SET_BOOTSTRAP=file|fold_at_height`; `fold_at_height`
    // uses `bk_set_at_height` to reconstruct the committee at
    // `BRIDGE_BK_SET_TARGET_SEQNO` (falls back to `bootstrap_seqno`)
    // by folding `bkSetUpdates` onto the JSON genesis anchor.
    let bk_set_config_str = bk_set_config
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("bk_set_config path not UTF-8"))?
        .to_string();
    let bk_set = bk_set_bootstrap::load_bk_set(&gql, &bk_set_config_str, bootstrap_seqno)
        .await
        .map_err(|e| anyhow::anyhow!("load BK set: {e}"))?;

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
                    "prover_bk_set commitment {} disagrees with prover_state {} — delete BOTH \
                     under {} or restore a paired backup",
                    hex::encode(loaded.commitment),
                    hex::encode(state.stored_bk_set_commitment),
                    prover_state_dir.display(),
                );
            }
            loaded
        },
        None => {
            let pbs = ProverBkSet::from_pubkeys(&bk_set, 0);
            pbs.save(bk_path_str)
                .map_err(|e| anyhow::anyhow!("save ProverBkSet: {e}"))?;
            info!(
                signers = pbs.pubkeys_hex.len(),
                "bootstrapped prover_bk_set.json"
            );
            pbs
        },
    };

    // Shared startup guard (parity with bridge-prover-daemon): reject the
    // "stale ./state on top of a re-initialised chain" configuration
    // before we hand off to LiveProverDriver.
    bk_set_bootstrap::verify_prover_bk_set_matches_config_file(&bk_set_config_str, &prover_bk_set)?;

    // Since bridge-prover-lib's 2026-07-27 refactor, `LiveProverDriver`
    // owns `prover_bk_set` as the sole BK-pubkey source and derives its
    // in-driver pubkey table on demand via `prover_bk_set.pubkeys()`.
    // The relayer no longer passes a separate `bk_set` argument.

    // ── Startup routing ────────────────────────────────────────────
    // Read the *full* on-chain state (four scalars + all 10 layer
    // windows) once, then hand it to `startup_decide::decide()` which
    // returns one of {Cold, WarmResume, Resurrect, Stop}. This replaces
    // both the old 3-arm seed_policy match AND the late "startup
    // drift" bail below — a shared test bridge that a co-tester has
    // advanced now cleanly resurrects instead of aborting.
    let bridge_probe = {
        let signer: PrivateKeySigner = private_key.parse()?;
        let probe = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let chain_id = probe.get_chain_id().await?;
        let wallet = EthereumWallet::from(signer.with_chain_id(Some(chain_id)));
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(rpc_url.parse()?);
        EthBridgeClient::new(bridge_address, provider)
    };
    let chain_full = bridge_probe
        .read_full_state()
        .await
        .map_err(|e| anyhow::anyhow!("read_full_state (startup): {e}"))?;
    info!(
        chain_last_seen = chain_full.last_seen_block_seq_no,
        chain_bk_upd = chain_full.last_bk_set_update_seq_no,
        local_last_seen = state.stored_last_seen_block_seq_no,
        local_bk_upd = state.stored_last_bk_set_update_seq_no,
        local_initialized = state.initialized,
        anchor_level = anchor_mode.level(),
        bundle_stride = anchor_mode.stride(),
        "startup: read on-chain state for routing",
    );
    // Anchor-mode maturity marker (Sergey's PR#35 review fallback for #2).
    // L2 has shellnet Deploy #12 smoke but no continuous-production stress
    // run yet — loud on startup so operators know what regime they're in.
    if matches!(anchor_mode, bridge_prover_lib::AnchorMode::L2) {
        tracing::warn!(
            "L2 anchoring is SMOKE-PENDING: shellnet Deploy #12 (2026-08-18) verified cold-start \
             end-to-end; no continuous multi-day production run yet. See \
             crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md Case 7 \
             for the expected log signature (watch for `layers=2` on the first Circuit 2 bundle) \
             and drift-recovery deltas. Report anomalies against that signature."
        );
    }
    let decision = bridge_relayer_daemon::startup_decide(bridge_relayer_daemon::DecideInputs {
        local: &state,
        chain: &chain_full,
        bootstrap_seqno,
        window_size: HISTORY_WINDOW_SIZE as usize,
        anchor_level: anchor_mode.level(),
    });
    let (state, seed_policy) = match decision {
        bridge_relayer_daemon::StartupDecision::Cold {
            policy,
        } => {
            info!(
                ?policy,
                "startup: Cold — contract at genesis, bootstrapping"
            );
            (state, policy)
        },
        bridge_relayer_daemon::StartupDecision::WarmResume => {
            info!(
                last_seen = state.stored_last_seen_block_seq_no,
                "startup: WarmResume — local state matches chain byte-for-byte",
            );
            (state, SeedPolicy::Resume)
        },
        bridge_relayer_daemon::StartupDecision::Resurrect {
            fresh_state,
        } => {
            let rebuilt = *fresh_state;
            info!(
                chain_last_seen = chain_full.last_seen_block_seq_no,
                "startup: Resurrect — rebuilding BridgeState from on-chain snapshot",
            );
            // Persist the resurrected state atomically before we build
            // the driver, so a subsequent crash-and-restart sees a
            // matching local mirror (which would then take the
            // WarmResume path).
            rebuilt
                .save(state_path_str)
                .map_err(|e| anyhow::anyhow!("save resurrected BridgeState: {e}"))?;
            (rebuilt, SeedPolicy::Resume)
        },
        bridge_relayer_daemon::StartupDecision::Stop {
            reason,
        } => {
            anyhow::bail!("startup routing STOP: {reason}");
        },
    };
    info!(?seed_policy, "LiveProverDriver seed policy");

    let driver = LiveProverDriver::new(gql, key_manager, state, prover_bk_set, LiveProverConfig {
        seed_policy,
        // Emit Poseidon-transcript proofs directly. Consumed in-process
        // by `bridge_snark_wrap::wrap_poseidon_snark_in_memory` — replaces
        // the old `export-1a1b2-poseidon-snark` subprocess that
        // independently re-fetched + re-proved every bundle.
        transcript: TranscriptKind::Poseidon,
        anchor_mode,
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!("LiveProverDriver::new: {e}"))?;
    let driver = Arc::new(Mutex::new(driver));

    let live_source = Arc::new(LiveBlockSource::new(Arc::clone(&driver), state_paths));

    // Re-use the same `bridge_probe` client we built for `read_full_state`
    // above — one signer/provider triple for the whole daemon lifetime.
    // The pre-decide drift-audit that used to live here is now subsumed
    // by `startup_decide::decide()`: `Stop` bails with a precise reason,
    // `Resurrect` rebuilds the local mirror before the driver is even
    // constructed, and `WarmResume` is only reached when local and chain
    // match byte-for-byte.
    let bridge = Arc::new(bridge_probe);

    // Anchor-level cross-check. Decision logic and the full truth table
    // live in `bridge_prover_lib::AnchorMode::verify_state_level` (§3 of
    // `l2_anchoring_implementation_plan.md`); this call site only formats
    // the operator-facing diagnostic.
    //
    // Skip on uninitialized state — the seed will stamp the correct level on
    // its first `apply`. `state` was moved into `LiveProverDriver::new`; we
    // read the post-decide view via the live-source snapshot. Chain-side
    // stride check is re-wired to `chain_full` (already read for routing
    // above; the old pre-decide `bridge.read_state()` is gone).
    let driver_state = live_source.driver_snapshot().await;
    if driver_state.initialized {
        if let Err(drift) = anchor_mode.verify_state_level(driver_state.anchor_level) {
            anyhow::bail!(
                "startup drift: state anchor_level={} but daemon configured for L{} \
                 (BRIDGE_ANCHOR_LEVEL / --anchor-level). Rename {} to \
                 state.pre_L{cfg_level}_$(date +%Y%m%d_%H%M%S) and rebootstrap. Never \
                 auto-migrate between anchor levels on a live bridge — a mid-run flip would \
                 submit a verifyBlock against the wrong on-chain window.",
                drift.state_level,
                drift.cfg_level,
                prover_state_dir.display(),
                cfg_level = drift.cfg_level,
            );
        }
        // Chain-side sanity: the on-chain lastSeenBlockSeqNo must sit on a
        // stride-aligned boundary for the level we think we're running.
        // If not, this is a strong signal we're pointing at a bridge that
        // was deployed under a different level.
        if chain_full.last_seen_block_seq_no != 0
            && chain_full.last_seen_block_seq_no % anchor_mode.stride() != 0
        {
            anyhow::bail!(
                "on-chain last_seen_block_seq_no={} is not a multiple of the L{} bundle stride {} \
                 — either the bridge was deployed under a different anchor level, or this daemon \
                 is pointed at the wrong contract.",
                chain_full.last_seen_block_seq_no,
                anchor_mode.level(),
                anchor_mode.stride(),
            );
        }

        // Chain-shape vs anchor-level mismatch (both directions) is caught
        // inside `startup_decide::decide()` before Resurrect persists any
        // state — see the top-of-decide gate in `startup_decide.rs`.
    }

    let cfg = RelayerConfig::new(&state_path);

    // Wrap the live source so the two proof-byte fields in AnBlockData carry
    // Poseidon R15 SHPLONK calldata instead of the daemon's raw halo2 bytes.
    // The wrapper delegates ack / driver_snapshot back to LiveBlockSource so
    // state persistence and ack-after-submit semantics are unchanged.
    // Aggregation is unconditional: the on-chain `AckiNackiBridge` verifier
    // only accepts aggregated calldata (raw halo2 bytes revert with
    // `AttestationProofRejected()`).
    use bridge_relayer_daemon::{
        AggregatedBlockSource, Circuit12ShplonkPipeline, PoseidonSnarkWrapper,
        SubprocessAggregator, SubprocessAggregatorConfig,
    };
    let aggregator_cfg = SubprocessAggregatorConfig::new(
        &aggregation.aggregator_dir,
        &aggregation.verifiers_dir,
        &params_dir,
    )
    .with_pk_cache_dir(&aggregation.pk_cache_dir);
    let pipeline = Circuit12ShplonkPipeline::new(
        PoseidonSnarkWrapper::new(&params_dir),
        SubprocessAggregator::new(aggregator_cfg),
    );
    let aggregated = Arc::new(AggregatedBlockSource::new(
        Arc::clone(&live_source),
        pipeline,
    ));
    info!(
        aggregator_dir = %aggregation.aggregator_dir.display(),
        verifiers_dir = %aggregation.verifiers_dir.display(),
        pk_cache_dir = %aggregation.pk_cache_dir.display(),
        "daemon-live: C1/C2 R15 SHPLONK aggregation",
    );
    spawn_and_run(
        cfg,
        Arc::clone(&aggregated),
        aggregated,
        bridge,
        backoff,
        &gql_endpoint,
        &params_dir,
        &prover_state_dir,
    )
    .await
}

/// Build a Relayer, run the startup drift audit, then drive
/// `run_until_shutdown`. Generic over the source types so both aggregation-on
/// and aggregation-off paths in [`run_daemon_live`] share the same startup +
/// run wiring.
#[allow(clippy::too_many_arguments)]
async fn spawn_and_run<S, U, B>(
    cfg: RelayerConfig,
    source: Arc<S>,
    bk_update_source: Arc<U>,
    bridge: Arc<B>,
    backoff: BackoffConfig,
    gql_endpoint: &str,
    params_dir: &Path,
    prover_state_dir: &Path,
) -> anyhow::Result<()>
where
    S: bridge_relayer_daemon::source::BlockSource + 'static,
    U: bridge_relayer_daemon::source::BkUpdateSource + 'static,
    B: bridge_relayer_daemon::bridge::BridgeClient + 'static,
{
    let mut relayer = Relayer::new(cfg, source, bk_update_source, bridge)?;

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

/// Serve the Prometheus text format at `http://<addr>/metrics` for the
/// lifetime of the process. Every `metrics::*` macro in this binary and in
/// its library crates (e.g. `relayer_gql_*` in `bridge-gql-fetcher`) records
/// into this exporter. Histogram buckets cover sub-second RPC round-trips up
/// to ten-minute proving stages.
fn install_metrics_exporter(addr: std::net::SocketAddr) -> anyhow::Result<()> {
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
    info!(%addr, "metrics exporter listening (GET /metrics)");
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
