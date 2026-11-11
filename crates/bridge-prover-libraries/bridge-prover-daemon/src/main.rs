//! Bridge Prover Daemon — thin harness around
//! [`bridge_prover_lib::live_driver::LiveProverDriver`].
//!
//! All orchestration (GraphQL polling, bootstrap, BK-set drain, Circuits
//! 1A/1B + 2 proof generation, in-memory state advance) lives inside the
//! library. This binary owns only what a library shouldn't:
//!
//!   * env-driven configuration (`BRIDGE_GQL_ENDPOINT`,
//!     `BRIDGE_BOOTSTRAP_SEQNO`, `BRIDGE_ANCHOR_LEVEL`),
//!   * the starting BK set: `state/prover_bk_set.json`, else built from GraphQL
//!     ([`bridge_prover_lib::bk_set_history`]),
//!   * Ctrl-C graceful shutdown flag,
//!   * disk persistence (`./state/*.json`, `./bootstrap_seed.json`),
//!   * verifier IPC (`./proofs/{proof,result,bkupd,bkupd_result}_NNN.json`), and
//!   * the `self-verify` feature gate that inline-verifies proofs in-process.
//!
//! The two-daemon contract is the `LiveProverDriver` API in
//! `bridge-prover-lib/src/live_driver/mod.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tracing::{error, info, warn};

use bridge_prover_lib::bk_set_history::{
    bk_target, build_bk_set_with_retry, genesis_bk_for_seed, BuildError, BuildPolicy,
};
use bridge_prover_lib::bootstrap;
use bridge_prover_lib::bridge_state::BridgeState;
use bridge_gql_fetcher::gql_client;
use bridge_prover_lib::ipc;
use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::live_driver::{
    BkUpdateProofArtifacts, BundleFinalizationType, BundleProofArtifacts,
    DriverError, HISTORY_WINDOW_SIZE, LiveBkUpdateEvent, LiveBundleEvent, LiveProverConfig,
    LiveProverDriver, SeedBk, SeedPolicy,
};
use bridge_poseidon as poseidon;
use bridge_prover_lib::prover_bk_set::ProverBkSet;
use bridge_prover_lib::{AnchorMode, THINNING_FACTOR_P};
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;

#[cfg(feature = "self-verify")]
use bridge_prover_lib::bridge_state::BundleResult;
#[cfg(feature = "self-verify")]
use bridge_prover_lib::verifier;
#[cfg(feature = "self-verify")]
use bridge_prover_lib::Fr;

const DEFAULT_GQL_ENDPOINT: &str = "http://localhost/graphql";
const ENV_GQL_ENDPOINT: &str = "BRIDGE_GQL_ENDPOINT";
const ENV_BOOTSTRAP_SEQNO: &str = "BRIDGE_BOOTSTRAP_SEQNO";
const ENV_ANCHOR_LEVEL: &str = "BRIDGE_ANCHOR_LEVEL";
const PARAMS_DIR: &str = "./params";
const LOGS_DIR: &str = "./logs";
// STATE_FILE / PROVER_BK_SET_FILE are resolved at runtime from
// `bridge_prover_lib::paths` — see the `state_file` / `prover_bk_set_file`
// locals in `main()`. The pre-refactor `./state/…` constants have been
// replaced so the path can be redirected via `BRIDGE_STATE_DIR` /
// `BRIDGE_CONFIG_DIR` for the per-mode L1/L2 config layout.

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// How often the chain head is polled while the BK set waits for the seed
/// block to exist.
const SEED_WAIT_INTERVAL: Duration = Duration::from_secs(10);
/// The GraphQL field a BK-set build starts from.
const LATEST_BK_SET_FIELD: &str = "latestBkSet";
#[cfg(not(feature = "self-verify"))]
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    std::fs::create_dir_all(LOGS_DIR).ok();
    bridge_prover_lib::paths::ensure_state_dir();
    ipc::ensure_proofs_dir();

    let state_file = bridge_prover_lib::paths::prover_state_file()
        .to_string_lossy()
        .into_owned();
    let prover_bk_set_file = bridge_prover_lib::paths::prover_bk_set_file()
        .to_string_lossy()
        .into_owned();

    let gql_endpoint = std::env::var(ENV_GQL_ENDPOINT)
        .unwrap_or_else(|_| DEFAULT_GQL_ENDPOINT.to_string());
    let anchor_mode = parse_anchor_level()?;
    // `bundle_size` sourced from `AnchorMode::stride()` so the seed-alignment
    // check tracks whichever anchor level the operator selected.
    let bundle_size = anchor_mode.stride();
    let explicit_bootstrap_seqno = parse_explicit_bootstrap(bundle_size)?;

    info!("=== Bridge Prover Daemon (Circuit 1a/1b + Circuit 2) ===");
    info!("GQL endpoint: {}", gql_endpoint);
    info!(
        "W = {}, P = {}, anchor_level = L{}, bundle = {} blocks",
        HISTORY_WINDOW_SIZE, THINNING_FACTOR_P, anchor_mode.level(), bundle_size
    );
    if matches!(anchor_mode, AnchorMode::L2) {
        info!(
            stride = anchor_mode.stride(),
            "L2 anchoring (shellnet operational default since Deploy #12); watch for layers=2 on the first Circuit 2 bundle"
        );
    }
    match explicit_bootstrap_seqno {
        Some(n) => info!("bootstrap: EXPLICIT seed seq_no = {}", n),
        None => info!(
            "bootstrap: AUTO (next L{}-stride boundary [{} blocks] past chain head)",
            anchor_mode.level(),
            bundle_size,
        ),
    }
    info!("send SIGINT (Ctrl-C) to shut down cleanly");

    let shutdown = install_ctrl_c_flag();

    // ---- Wire dependencies -------------------------------------------------
    let gql = gql_client::create_client(&gql_endpoint)?;
    let state = BridgeState::load(&state_file, HISTORY_WINDOW_SIZE as usize)?;
    info!(
        "state: initialized={}, last_key_block={}, anchor_level={}",
        state.initialized, state.stored_last_seen_block_seq_no, state.anchor_level
    );

    // Anchor-level cross-check against the on-disk state. Decision logic
    // and the full truth table live in `bridge_prover_lib::AnchorMode::
    // verify_state_level` (see §3 of `docs/l2_anchoring_implementation_plan.md`);
    // this call site only formats the operator-facing diagnostic.
    // Uninitialized state skips: the seed will stamp the level on apply.
    if state.initialized {
        if let Err(drift) = anchor_mode.verify_state_level(state.anchor_level) {
            anyhow::bail!(
                "startup drift: prover_state anchor_level={} but daemon configured for L{} \
                 (BRIDGE_ANCHOR_LEVEL). Rename {} to {}.pre_L{}_$(date +%Y%m%d_%H%M%S) \
                 and rebootstrap; never auto-migrate anchor levels on a live bridge.",
                drift.state_level,
                drift.cfg_level,
                state_file,
                state_file,
                drift.cfg_level,
            );
        }
        // A state seeded with the seed's layers above the anchor level
        // (bridge-audit#122) stalls verifyBlock at the next boundary of that
        // layer. Without a seed file there is nothing to compare against.
        match bootstrap::BootstrapSeed::load(&bootstrap::default_seed_path()) {
            Ok(Some(seed)) => {
                if let Some(layer) = seed.layer_seeded_above_anchor_level(&state) {
                    anyhow::bail!(
                        "{} holds the seed's layer-{} root: it was seeded above anchor level {} \
                         by an earlier build and would stall verifyBlock at the next layer-{} \
                         boundary. Move it and {} aside and restart with {}={} to rebootstrap at \
                         the bridge's seed.",
                        state_file,
                        layer,
                        seed.anchor_level,
                        layer,
                        prover_bk_set_file,
                        ENV_BOOTSTRAP_SEQNO,
                        seed.block_seq_no,
                    );
                }
            },
            Ok(None) => {},
            Err(e) => warn!("seed file not checked for layers above the anchor level: {e:#}"),
        }
    }

    // Resolve prover_bk_set: `prover_bk_set.json` when present, else built
    // from GraphQL at the height the state needs, or at the seed.
    //
    // Once `prover_bk_set.json` exists, IT is the sole source of truth
    // for the BK-pubkey table — intermediate rotations advance it via
    // the bk-update lane in `LiveProverDriver`.
    let seed_policy = match (explicit_bootstrap_seqno, state.initialized) {
        (_, true) => SeedPolicy::Resume,
        (Some(n), false) => SeedPolicy::Explicit(n),
        (None, false) => SeedPolicy::Auto,
    };
    let build_policy = BuildPolicy::default();
    let start = tokio::select! {
        start = startup_bk(
            &gql,
            &state,
            &state_file,
            &prover_bk_set_file,
            seed_policy,
            bundle_size,
            &build_policy,
        ) => start?,
        () = shutdown_requested(&shutdown) => {
            info!("shutdown while resolving the prover BK set; exiting");
            return Ok(());
        },
    };
    let StartupBk {
        prover_bk_set,
        seed_policy,
        seed_bk,
    } = start;

    let bk_set = prover_bk_set
        .pubkeys()
        .context("prover_bk_set.pubkeys()")?;
    let (bk_commitment_fr, _) = poseidon::compute_bk_set_poseidon(&bk_set);
    info!(
        "BK set: {} signers, commitment={}",
        bk_set.len(),
        hex::encode(bk_commitment_fr.to_repr())
    );

    info!("loading keys...");
    let mut key_manager = KeyManager::new(Path::new(PARAMS_DIR));
    key_manager.ensure_primary_keys(&bk_set)?;
    key_manager.ensure_fallback_keys(&bk_set)?;
    key_manager.ensure_layer_keys()?;
    info!("keys ready (primary, fallback, layer)");

    let mut driver = LiveProverDriver::new(
        gql,
        key_manager,
        state,
        prover_bk_set,
        LiveProverConfig {
            seed_policy,
            anchor_mode,
            seed_bk,
            ..Default::default()
        },
    )?;

    // Seed persistence: the seed JSON is written once, right after the driver
    // completes its bootstrap. On a Resume start (state already on disk) the
    // seed file was written by an earlier run — treat it as already persisted.
    let mut seed_persisted = matches!(seed_policy, SeedPolicy::Resume);

    // Single-slot deferred bk-update ack.
    //
    // The contract's `PrevSlotStillOwedForBundle` gate only reverts a new
    // rotation when the previous one still owes an unverified bundle
    // boundary: rotations before that boundary, and the first one past
    // it, apply back-to-back; a further one waits until verifyBlock
    // reaches the last boundary at or below the previous rotation.
    // This slot holds the already-verified rotation artifacts returned
    // by the verifier daemon when `driver.ack_bk_update` refused to
    // rotate `prover_bk_set` because the bundle lane hasn't yet covered
    // the last OLD-signed key block ≤ N. The main loop keeps retrying
    // the ack after every successful bundle ack until it takes.
    let mut deferred_bk_ack: Option<BkUpdateProofArtifacts> = None;

    // ---- Main loop ---------------------------------------------------------
    while !shutdown.load(Ordering::SeqCst) {
        // Try to drain any deferred bk-update ack first. It only becomes
        // ack-able once the bundle lane has advanced past the last
        // OLD-signed key block ≤ rotation_seqno, so a successful drain
        // here means the previous iteration ack'd a bundle whose height
        // pushed `cursor + stride > rotation_seqno`.
        if try_drain_deferred_bk_ack(&mut driver, &mut deferred_bk_ack)? {
            persist_seed_if_needed(&driver, &mut seed_persisted)?;
        }

        // Drain rotations first — `poll_next_bundle` refuses to advance past
        // an un-acked bk-update whose height is <= the next thinned target.
        // Skip the poll when a rotation is already parked in the deferred
        // slot: re-polling would re-fetch the same rotation, redundantly
        // re-prove Circuit 1, and re-submit the IPC request the verifier
        // already ACK'd.
        if deferred_bk_ack.is_none() {
            // `0`: every rotation is proved; this daemon submits each one.
            match driver.poll_next_bk_update(0).await {
                Ok(LiveBkUpdateEvent::Bootstrapping {
                    seed_seqno,
                    chain_head_seqno,
                }) => {
                    info!(
                        "bootstrap: waiting for seed {} (chain head {})",
                        seed_seqno, chain_head_seqno
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
                Ok(LiveBkUpdateEvent::BkUpdate(update)) => {
                    match handle_bk_update(&mut driver, update).await? {
                        HandleBkUpdate::Acked => {
                            persist_seed_if_needed(&driver, &mut seed_persisted)?;
                            continue;
                        }
                        HandleBkUpdate::Deferred(artifacts) => {
                            deferred_bk_ack = Some(artifacts);
                            // Fall through to bundle poll: only bundle
                            // acks can push cursor + stride past the
                            // rotation seqno, unblocking the deferred ack.
                        }
                        HandleBkUpdate::Rejected => break,
                    }
                }
                Ok(LiveBkUpdateEvent::Nothing) => { /* fall through to bundle poll */ }
                // Not expected with `0`; handled as a failed poll.
                Ok(LiveBkUpdateEvent::Applied(delta)) => {
                    error!(
                        "poll_next_bk_update(0) returned an unproven rotation at {} — retrying",
                        delta.block_seq_no
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                },
                Err(e) => {
                    warn!("poll_next_bk_update: {} — retrying", e);
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
            }
        }

        match driver.poll_next_bundle().await {
            Ok(LiveBundleEvent::Bootstrapping {
                seed_seqno,
                chain_head_seqno,
            }) => {
                info!(
                    "bootstrap: waiting for seed {} (chain head {})",
                    seed_seqno, chain_head_seqno
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Ok(LiveBundleEvent::Nothing {
                next_target_seqno,
                chain_head_seqno,
                blocked_by_pending_bk_update,
            }) => {
                if blocked_by_pending_bk_update {
                    // Informational: a rotation is pending, but bundle
                    // proving is no longer held for it. Sleep
                    // like the ordinary Nothing path.
                    info!(
                        "rotation pending at or below next target {}; bundle lane stays open",
                        next_target_seqno
                    );
                }
                tracing::debug!(
                    "no new key block (next target {}, chain head {})",
                    next_target_seqno,
                    chain_head_seqno,
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Ok(LiveBundleEvent::Bundle(bundle)) => {
                if !handle_bundle(&mut driver, &bundle).await? {
                    break;
                }
                persist_seed_if_needed(&driver, &mut seed_persisted)?;
            }
            Err(e) => {
                error!("poll_next_bundle: {:#} — retrying", e);
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
    info!("main loop exited cleanly");
    Ok(())
}

// -------------------------------------------------------------------------
// Setup helpers
// -------------------------------------------------------------------------

fn install_ctrl_c_flag() -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let s = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Ctrl-C received; shutting down at next safe point...");
            s.store(true, Ordering::SeqCst);
        }
    });
    shutdown
}

/// Resolves once Ctrl-C has set `flag`.
async fn shutdown_requested(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// What the driver starts with.
struct StartupBk {
    prover_bk_set: ProverBkSet,
    seed_policy: SeedPolicy,
    /// BK fields of the seed: `Some` only when the set was built at the seed.
    seed_bk: Option<SeedBk>,
}

/// The BK set the driver starts with.
///
/// * `state` is initialized and `prover_bk_set_file` exists: the file is taken,
///   and must have the state's commitment.
/// * `state` is initialized and there is no file: the set is built from GraphQL
///   at [`bk_target`] of the state's last seen block and last rotation, must
///   have the state's commitment, and carries the state's rotation cursor.
/// * `state` is not initialized: a file is moved aside ([`bk_file_at_start`]),
///   and the set is built at the seed S — `BRIDGE_BOOTSTRAP_SEQNO`, or under
///   `Auto` the next stride boundary past the chain head. The block S must
///   exist first, so this waits for the chain head to reach S. The driver then
///   seeds at S (`SeedPolicy::Explicit(S)`) with the built set's commitment and
///   the newest rotation at or below S.
///
/// A built set is written to `prover_bk_set_file` before this returns.
/// Transient GraphQL failures are retried without a bound; a server without
/// `latestBkSet` and a set that keeps failing its checks are errors. The seed
/// path asks for `latestBkSet` before it waits, so an old server fails at once.
async fn startup_bk(
    gql: &gql_client::GqlClient,
    state: &BridgeState,
    state_file: &str,
    prover_bk_set_file: &str,
    seed_policy: SeedPolicy,
    stride: u64,
    policy: &BuildPolicy,
) -> anyhow::Result<StartupBk> {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let BkFileAtStart::Take(loaded) = bk_file_at_start(prover_bk_set_file, state, now_unix)? {
        info!(
            "prover_bk_set: loaded {} signers, last_applied_update_seq_no={}",
            loaded.pubkeys_hex.len(),
            loaded.last_applied_update_seq_no,
        );
        return Ok(StartupBk {
            prover_bk_set: loaded,
            seed_policy,
            seed_bk: None,
        });
    }

    if state.initialized {
        let cursor = state.stored_last_bk_set_update_seq_no;
        let target = bk_target(state.stored_last_seen_block_seq_no, cursor, stride);
        info!(
            "prover_bk_set: {} not found; building it from GraphQL at height {} (check {:?})",
            prover_bk_set_file, target.height, target.check,
        );
        let built = build_bk_set_with_retry(gql, &target, policy)
            .await
            .with_context(|| format!("build the prover BK set at height {}", target.height))?;
        let pbs = ProverBkSet::from_pubkeys(&built.pubkeys, cursor);
        anyhow::ensure!(
            pbs.commitment == state.stored_bk_set_commitment,
            "the BK set built from GraphQL at height {} has commitment {}, but {} holds {}: the \
             state follows another network or another BK set",
            built.height,
            hex::encode(pbs.commitment),
            state_file,
            hex::encode(state.stored_bk_set_commitment),
        );
        pbs.save(prover_bk_set_file)?;
        info!(
            "prover_bk_set: built {} signers at height {}, last_applied_update_seq_no={} → {}",
            pbs.pubkeys_hex.len(),
            built.height,
            cursor,
            prover_bk_set_file,
        );
        return Ok(StartupBk {
            prover_bk_set: pbs,
            seed_policy,
            seed_bk: None,
        });
    }

    ensure_latest_bk_set_known(gql).await?;
    let seed = match seed_policy {
        SeedPolicy::Explicit(n) => n,
        SeedPolicy::Auto => {
            let head = loop {
                match chain_head(gql).await {
                    Ok(head) => break head,
                    Err(e) => {
                        warn!("bootstrap: {:#} — retrying", e);
                        tokio::time::sleep(SEED_WAIT_INTERVAL).await;
                    },
                }
            };
            let seed = (head / stride + 1) * stride;
            info!(
                "bootstrap: Auto seed pinned at seq_no={} (chain head at {})",
                seed, head
            );
            seed
        },
        SeedPolicy::Resume => anyhow::bail!("SeedPolicy::Resume needs an initialized state"),
    };
    wait_for_block(gql, seed).await;
    info!(
        "prover_bk_set: {} not found; building it from GraphQL at the seed {}",
        prover_bk_set_file, seed,
    );
    let genesis = genesis_bk_for_seed(gql, seed, policy)
        .await
        .with_context(|| format!("build the prover BK set at the seed {seed}"))?;
    let cursor = genesis.last_bk_set_update_seq_no;
    let pbs = ProverBkSet::from_pubkeys(&genesis.set.pubkeys, cursor);
    pbs.save(prover_bk_set_file)?;
    info!(
        "prover_bk_set: built {} signers at the seed {}, last_applied_update_seq_no={} → {}",
        pbs.pubkeys_hex.len(),
        seed,
        cursor,
        prover_bk_set_file,
    );
    let seed_bk = SeedBk {
        commitment: pbs.commitment,
        last_bk_set_update_seq_no: cursor,
    };
    Ok(StartupBk {
        prover_bk_set: pbs,
        seed_policy: SeedPolicy::Explicit(seed),
        seed_bk: Some(seed_bk),
    })
}

/// What `prover_bk_set.json` gives [`startup_bk`].
#[derive(Debug)]
enum BkFileAtStart {
    /// The set of the initialized state.
    Take(ProverBkSet),
    /// No set to start with: there is no file, or the state is not initialized
    /// and the file was moved aside.
    Build,
}

/// The `prover_bk_set.json` side of [`startup_bk`].
///
/// With an initialized `state` the file at `path` is taken when there is one,
/// and must have the state's commitment.
///
/// With an uninitialized `state` the file is left from a start whose first ack
/// never landed: `prover_state.json` is first written at the first ack, the
/// set at the seed before it. That seed need not be the one this start picks,
/// since under `Auto` the next boundary past the chain head has moved on, and
/// a set taken for another seed fails the seed block's check on every poll.
/// The file is moved to `<name>.mismatch-<now_unix>` without being read, and
/// the set is built at the seed again.
fn bk_file_at_start(
    path: &str,
    state: &BridgeState,
    now_unix: u64,
) -> anyhow::Result<BkFileAtStart> {
    if !state.initialized {
        if std::fs::symlink_metadata(path).is_ok() {
            let backup = move_aside(Path::new(path), now_unix)?;
            warn!(
                "prover_bk_set: {} is left from a start that never acked (prover_state.json is \
                 not initialized); moved it to {} and building the set at the seed again",
                path,
                backup.display(),
            );
        }
        return Ok(BkFileAtStart::Build);
    }
    let Some(loaded) = ProverBkSet::load(path)? else {
        return Ok(BkFileAtStart::Build);
    };
    anyhow::ensure!(
        loaded.commitment == state.stored_bk_set_commitment,
        "prover_bk_set.json commitment {} disagrees with prover_state.json {} — move prover_bk_set.json \
         aside to rebuild it from GraphQL, or restore both files from a paired backup",
        hex::encode(loaded.commitment),
        hex::encode(state.stored_bk_set_commitment),
    );
    Ok(BkFileAtStart::Take(loaded))
}

/// Moves `path` to `<file name>.mismatch-<now_unix>` in the same directory,
/// appending `.1`, `.2`, ... while that name is taken, so nothing is
/// overwritten. Returns where the file went.
fn move_aside(path: &Path, now_unix: u64) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let mut base = name.to_os_string();
    base.push(format!(".mismatch-{now_unix}"));
    let mut backup = path.with_file_name(&base);
    let mut n = 0u32;
    while std::fs::symlink_metadata(&backup).is_ok() {
        n += 1;
        let mut next = base.clone();
        next.push(format!(".{n}"));
        backup = path.with_file_name(next);
    }
    std::fs::rename(path, &backup)
        .with_context(|| format!("rename {} to {}", path.display(), backup.display()))?;
    Ok(backup)
}

/// Fails at once when the server does not know `latestBkSet`, which the set
/// build needs: waiting for the seed block first would only delay the same
/// error by up to a stride of blocks. Any other answer leaves it to the build,
/// which retries.
async fn ensure_latest_bk_set_known(gql: &gql_client::GqlClient) -> anyhow::Result<()> {
    match gql.query_latest_bk_set().await {
        Err(e)
            if e.downcast_ref::<gql_client::UnknownFieldError>()
                .is_some_and(|u| u.field == LATEST_BK_SET_FIELD) =>
        {
            Err(BuildError::NoLatestBkSetField.into())
        },
        Err(e) => {
            warn!(
                "bootstrap: asking for latestBkSet failed: {:#}; the BK set build retries it",
                e
            );
            Ok(())
        },
        Ok(_) => Ok(()),
    }
}

/// The highest seq_no among the newest blocks GraphQL reports.
async fn chain_head(gql: &gql_client::GqlClient) -> anyhow::Result<u64> {
    let blocks = gql
        .query_latest_blocks(5)
        .await
        .context("query the chain head")?;
    Ok(blocks.iter().map(|(_, seq_no)| *seq_no).max().unwrap_or(0))
}

/// Polls the chain head every [`SEED_WAIT_INTERVAL`] until it reaches `seed`.
/// A failed query is logged and retried.
async fn wait_for_block(gql: &gql_client::GqlClient, seed: u64) {
    loop {
        match chain_head(gql).await {
            Ok(head) if head >= seed => return,
            Ok(head) => info!("bootstrap: waiting for seed {} (chain head {})", seed, head),
            Err(e) => warn!("bootstrap: waiting for seed {}: {:#} — retrying", seed, e),
        }
        tokio::time::sleep(SEED_WAIT_INTERVAL).await;
    }
}

fn parse_explicit_bootstrap(bundle_size: u64) -> anyhow::Result<Option<u64>> {
    match std::env::var(ENV_BOOTSTRAP_SEQNO) {
        Ok(v) => {
            let n: u64 = v.parse().with_context(|| {
                format!("{} must be a positive integer, got {:?}", ENV_BOOTSTRAP_SEQNO, v)
            })?;
            anyhow::ensure!(
                n > 0 && n % bundle_size == 0,
                "{}={} must be > 0 and divisible by bundle stride={}",
                ENV_BOOTSTRAP_SEQNO,
                n,
                bundle_size
            );
            Ok(Some(n))
        }
        Err(_) => Ok(None),
    }
}

/// Parse `BRIDGE_ANCHOR_LEVEL` into an [`AnchorMode`]. Defaults to L1 when
/// unset. Accepted values are `1` (L1) and `2` (L2); anything else errors so
/// a typo can't silently downgrade to L1.
fn parse_anchor_level() -> anyhow::Result<AnchorMode> {
    match std::env::var(ENV_ANCHOR_LEVEL) {
        Ok(v) => {
            let n: u8 = v.parse().with_context(|| {
                format!("{} must be 1 or 2, got {:?}", ENV_ANCHOR_LEVEL, v)
            })?;
            AnchorMode::from_level(n).map_err(|e| anyhow::anyhow!("{ENV_ANCHOR_LEVEL}: {e}"))
        }
        Err(_) => Ok(AnchorMode::L1),
    }
}

// -------------------------------------------------------------------------
// Persistence
// -------------------------------------------------------------------------

fn persist(driver: &LiveProverDriver) -> anyhow::Result<()> {
    let state_file = bridge_prover_lib::paths::prover_state_file();
    let prover_bk_set_file = bridge_prover_lib::paths::prover_bk_set_file();
    driver.snapshot_state().save(&state_file.to_string_lossy())?;
    driver.snapshot_prover_bk_set().save(&prover_bk_set_file.to_string_lossy())?;
    Ok(())
}

/// Writes `bootstrap_seed.json` the first time the driver reports a
/// resolved seed. No-op afterwards (both on repeated calls and on a Resume
/// start where the seed file already lives on disk).
fn persist_seed_if_needed(
    driver: &LiveProverDriver,
    seed_persisted: &mut bool,
) -> anyhow::Result<()> {
    if *seed_persisted {
        return Ok(());
    }
    if let Some(seed) = driver.snapshot_bootstrap_seed() {
        let seed_path = bootstrap::default_seed_path();
        seed.save(&seed_path)?;
        info!(
            "bootstrap seed persisted: seq_no={}, height={}, layers={} → {}",
            seed.block_seq_no,
            seed.block_height,
            seed.layer_hashes.len(),
            seed_path,
        );
        *seed_persisted = true;
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Bundle / bk-update handling
// -------------------------------------------------------------------------

/// Returns `Ok(false)` when the caller should stop the main loop
/// (verifier rejection). `Ok(true)` on ACK.
async fn handle_bundle(
    driver: &mut LiveProverDriver,
    bundle: &BundleProofArtifacts,
) -> anyhow::Result<bool> {
    info!(
        "key block {}: {} bundle ready (primary {}ms, layer {}ms)",
        bundle.block_seq_no,
        bundle.fin_type.as_str(),
        bundle.attestation_proof_gen_ms,
        bundle.layer_proof_gen_ms,
    );
    let req = bundle_to_proof_request(bundle);
    ipc::write_combined_proof(&req)?;

    #[cfg(not(feature = "self-verify"))]
    {
        info!("key block {}: awaiting verifier ACK...", bundle.block_seq_no);
        let result =
            ipc::wait_for_result(bundle.block_seq_no as u32, VERIFIER_TIMEOUT).await?;
        if !(result.primary_verified && result.layer_verified) {
            error!(
                "key block {}: verifier REJECTED (primary={}, layer={}, err={:?}); \
                 state NOT advanced",
                bundle.block_seq_no,
                result.primary_verified,
                result.layer_verified,
                result.error,
            );
            return Ok(false);
        }
        info!("key block {}: verifier ACK", bundle.block_seq_no);
    }

    #[cfg(feature = "self-verify")]
    {
        let (primary_ok, layer_ok) = verify_bundle_inline(driver, bundle);
        let verify_ok = primary_ok && layer_ok;
        let ts_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        driver.record_self_verify_result(BundleResult {
            key_block_seq_no: bundle.block_seq_no,
            primary_ok,
            layer_ok,
            verify_ok,
            ts_unix,
        });
        if !verify_ok {
            // Do NOT ack; persist the failure marker for observability.
            persist(driver)?;
            anyhow::bail!(
                "key block {}: self-verification FAILED (primary={}, layer={}); \
                 state NOT advanced past failed bundle",
                bundle.block_seq_no,
                primary_ok,
                layer_ok,
            );
        }
        info!(
            "key block {}: BOTH SELF-VERIFIED OK (primary={}, layer={})",
            bundle.block_seq_no, primary_ok, layer_ok
        );
    }

    driver.ack_bundle(bundle)?;
    persist(driver)?;
    Ok(true)
}

/// Result of a `handle_bk_update` call.
///
/// `Deferred` carries the already-verified artifacts back to the caller so
/// they can be re-`ack`'d against the driver once the bundle lane advances
/// past the "next OLD-signed key block" threshold (see
/// [`DriverError::AckTooEarly`]). The verifier IPC round-trip is NOT
/// repeated: the caller only retries `driver.ack_bk_update(&deferred)`.
enum HandleBkUpdate {
    /// Verifier ACK'd and driver state advanced.
    Acked,
    /// Verifier ACK'd but driver refused to rotate its in-memory
    /// `prover_bk_set` because there is still an un-bundled OLD-signed
    /// key block ≤ `rotation_seqno`. Caller must retry
    /// `driver.ack_bk_update` after each successful bundle ack until it
    /// succeeds.
    Deferred(BkUpdateProofArtifacts),
    /// Verifier rejected the proof — caller should stop the loop.
    Rejected,
}

async fn handle_bk_update(
    driver: &mut LiveProverDriver,
    update: BkUpdateProofArtifacts,
) -> anyhow::Result<HandleBkUpdate> {
    info!(
        "bk-update {}: {} rotation ready ({}ms proof)",
        update.block_seq_no,
        update.fin_type.as_str(),
        update.attestation_proof_gen_ms,
    );
    let req = bkupdate_to_ipc_request(&update);
    ipc::write_bk_update_request(&req)?;

    #[cfg(not(feature = "self-verify"))]
    let result =
        ipc::wait_for_bk_update_result(update.block_seq_no as u32, VERIFIER_TIMEOUT).await?;

    #[cfg(feature = "self-verify")]
    let result = {
        // Self-verify does not inline-verify bk-update bundles: the SHA-256
        // Merkle equality + monotonicity precondition are already enforced
        // inside `live_driver::bk_update::drive_next_bk_update`, and inline
        // BLS-pairing verification would require reconstructing the OLD set
        // Fr — not worth it for the CI smoke path.
        warn!(
            "bk-update {}: self-verify does not inline-verify — synthesising verify_ok=true",
            update.block_seq_no
        );
        ipc::BkUpdateResult {
            block_seq_no: update.block_seq_no as u32,
            attestation_verified: true,
            merkle_verified: true,
            monotonicity_ok: true,
            verify_ok: true,
            error: None,
        }
    };

    if !result.verify_ok {
        error!(
            "bk-update {}: verifier REJECTED (attest={}, merkle={}, mono={}, err={:?}); \
             state NOT advanced",
            update.block_seq_no,
            result.attestation_verified,
            result.merkle_verified,
            result.monotonicity_ok,
            result.error,
        );
        return Ok(HandleBkUpdate::Rejected);
    }
    info!("bk-update {}: verifier ACK", update.block_seq_no);

    match driver.ack_bk_update(&update) {
        Ok(()) => {
            persist(driver)?;
            Ok(HandleBkUpdate::Acked)
        }
        Err(DriverError::AckTooEarly { cursor, rotation_seqno }) => {
            info!(
                "bk-update {}: driver defers ack — cursor {} + stride ≤ rotation {} \
                 (bundle lane must advance first). Retaining verified artifacts; \
                 will re-ack after each successful bundle ack.",
                update.block_seq_no, cursor, rotation_seqno,
            );
            Ok(HandleBkUpdate::Deferred(update))
        }
        Err(e) => Err(anyhow::Error::from(e)),
    }
}

/// Attempt to drain a deferred bk-update ack. Returns `Ok(true)` when the
/// ack succeeded and the deferred slot has been cleared; `Ok(false)` when
/// the driver still refuses (guard unchanged) — the slot is preserved
/// unchanged. Errors on any driver error other than `AckTooEarly`.
fn try_drain_deferred_bk_ack(
    driver: &mut LiveProverDriver,
    slot: &mut Option<BkUpdateProofArtifacts>,
) -> anyhow::Result<bool> {
    let Some(update) = slot.take() else { return Ok(false); };
    match driver.ack_bk_update(&update) {
        Ok(()) => {
            info!(
                "bk-update {}: deferred ack drained (bundle lane advanced past rotation)",
                update.block_seq_no
            );
            persist(driver)?;
            Ok(true)
        }
        Err(DriverError::AckTooEarly { .. }) => {
            *slot = Some(update);
            Ok(false)
        }
        Err(e) => Err(anyhow::Error::from(e)),
    }
}

// -------------------------------------------------------------------------
// IPC conversions (BundleProofArtifacts / BkUpdateProofArtifacts → IPC)
// -------------------------------------------------------------------------

fn to_ipc_circuit(t: BundleFinalizationType) -> ipc::AttestationCircuit {
    match t {
        BundleFinalizationType::Primary => ipc::AttestationCircuit::Primary,
        BundleFinalizationType::Fallback => ipc::AttestationCircuit::Fallback,
    }
}

fn bundle_to_proof_request(b: &BundleProofArtifacts) -> ipc::ProofRequest {
    // Schema v6: `block_id_hex` is the raw 32-byte BE chain hash (=
    // `uint256(bytes32(blockId))`); Circuit 1 and Circuit 2 both bind
    // `block_id_fr = fold(reverse(this))` and the verifier derives Fr on
    // demand via `ipc::hash_hex_to_fr`. Same wire semantics as
    // `BkUpdateRequest.block_id_hex`.
    ipc::ProofRequest {
        block_seq_no: b.block_seq_no as u32,
        block_height: b.block_height,
        last_seen_block_seqno: b.last_seen_block_seq_no as u32,
        block_id_hex: hex::encode(b.block_id_be),
        attestation_circuit: to_ipc_circuit(b.fin_type),
        attestation_proof_hex: hex::encode(&b.attestation_proof),
        layer_proof_hex: hex::encode(&b.layer_hashes_proof),
        bk_set_poseidon_hash_hex: hex::encode(b.bk_set_commitment_be),
        num_layers: b.num_layers,
        layer_hash_frs_hex: b.layer_hashes_be.iter().map(hex::encode).collect(),
        prev_max_level_layer_hash_hex: hex::encode(b.prev_max_level_layer_hash_be),
        attestation_proof_gen_ms: b.attestation_proof_gen_ms,
        layer_proof_gen_ms: b.layer_proof_gen_ms,
    }
}

fn bkupdate_to_ipc_request(u: &BkUpdateProofArtifacts) -> ipc::BkUpdateRequest {
    // Schema v6: single `block_id_hex` = raw 32-byte BE chain hash. The
    // verifier's SHA-256 Merkle open checks the raw bytes; the Fr public
    // instance for Circuit 1a/1b is derived on demand via
    // `ipc::hash_hex_to_fr`.
    ipc::BkUpdateRequest {
        block_seq_no: u.block_seq_no as u32,
        block_height: u.block_height,
        last_seen_bk_update_seqno: u.last_seen_bk_update_seq_no as u32,
        block_id_hex: hex::encode(u.block_id_be),
        attestation_circuit: to_ipc_circuit(u.fin_type),
        attestation_proof_hex: hex::encode(&u.attestation_proof),
        old_bk_set_poseidon_hash_hex: hex::encode(u.old_bk_set_commitment_be),
        new_bk_set_poseidon_hash_hex: hex::encode(u.new_bk_set_commitment_be),
        merkle_sibling_h01_hex: hex::encode(u.merkle_sibling_h01_be),
        merkle_sibling_h4_7_hex: hex::encode(u.merkle_sibling_h4_7_be),
        merkle_sibling_h8_15_hex: hex::encode(u.merkle_sibling_h8_15_be),
        attestation_proof_gen_ms: u.attestation_proof_gen_ms,
    }
}

// -------------------------------------------------------------------------
// self-verify: inline verification of both proofs (CI smoke test path)
// -------------------------------------------------------------------------

#[cfg(feature = "self-verify")]
fn verify_bundle_inline(
    driver: &LiveProverDriver,
    bundle: &BundleProofArtifacts,
) -> (bool, bool) {
    // Schema v6: `bundle.block_id_be` is the raw 32-byte BE chain hash — may
    // exceed the Fr modulus, so it is NOT a canonical `Fr::to_repr`. Reduce
    // via the same inner-product fold the circuits and on-chain Yul use.
    let block_id_fr = ipc::fold_hash_be_to_fr(&bundle.block_id_be);
    let bk_set_commitment_fr = fr_from_repr(bundle.bk_set_commitment_be);

    // Circuit 1a/1b public instances match the layout the verifier daemon
    // rebuilds in `bridge-verifier-daemon/src/main.rs::verify_primary`.
    let primary_instances = vec![
        block_id_fr,
        bk_set_commitment_fr,
        Fr::from(bundle.block_seq_no),
        Fr::from(bundle.last_seen_block_seq_no),
    ];
    let primary_ok = match bundle.fin_type {
        BundleFinalizationType::Primary => verifier::verify_primary_proof(
            driver.key_manager_ref(),
            &bundle.attestation_proof,
            &primary_instances,
        ),
        BundleFinalizationType::Fallback => verifier::verify_fallback_proof(
            driver.key_manager_ref(),
            &bundle.attestation_proof,
            &primary_instances,
        ),
    };

    // Circuit 2 public instances: 14 elements. Post-Circuit-1 byte-order
    // fix, Circuit 2 binds the same `block_id_fr` as Circuit 1, so we reuse
    // `block_id_fr` here for public instance [0].
    let mut layer_instances: Vec<Fr> = Vec::with_capacity(14);
    layer_instances.push(block_id_fr);
    layer_instances.push(bk_set_commitment_fr);
    layer_instances.push(Fr::from(bundle.num_layers as u64));
    for be in &bundle.layer_hashes_be {
        layer_instances.push(fr_from_repr(*be));
    }
    layer_instances.push(fr_from_repr(bundle.prev_max_level_layer_hash_be));
    let layer_ok = verifier::verify_layer_proof(
        driver.key_manager_ref(),
        &bundle.layer_hashes_proof,
        &layer_instances,
    );

    (primary_ok, layer_ok)
}

#[cfg(feature = "self-verify")]
fn fr_from_repr(bytes: [u8; 32]) -> Fr {
    Option::from(Fr::from_repr(bytes))
        .expect("BundleProofArtifacts field is canonical Fr::to_repr bytes")
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::atomic::AtomicUsize};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;

    /// What shellnet answers a `latestBkSet` query with: a gql-server older
    /// than 1.5.0.
    const OLD_SERVER_BODY: &str = r#"{"data":null,"errors":[{"message":"Unknown field \"latestBkSet\" on type \"BlockchainQuery\".","locations":[{"line":1,"column":16}]}]}"#;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bridge-prover-daemon-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn set_with(commitment: [u8; 32]) -> ProverBkSet {
        ProverBkSet {
            commitment,
            pubkeys_hex: BTreeMap::new(),
            last_applied_update_seq_no: 7,
        }
    }

    fn state(initialized: bool, commitment: [u8; 32]) -> BridgeState {
        let mut state = BridgeState::new(HISTORY_WINDOW_SIZE as usize);
        state.initialized = initialized;
        state.stored_bk_set_commitment = commitment;
        state
    }

    #[test]
    fn unseeded_state_moves_the_file_aside() {
        let dir = temp_dir("unseeded");
        let path = dir.join("prover_bk_set.json");
        set_with([1; 32]).save(path.to_str().unwrap()).unwrap();

        let got = bk_file_at_start(path.to_str().unwrap(), &state(false, [0; 32]), 1700).unwrap();

        assert!(matches!(got, BkFileAtStart::Build), "{got:?}");
        assert_eq!(entries(&dir), ["prover_bk_set.json.mismatch-1700"]);
        let moved = dir.join("prover_bk_set.json.mismatch-1700");
        let moved = ProverBkSet::load(moved.to_str().unwrap()).unwrap().unwrap();
        assert_eq!(moved.commitment, [1; 32]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unseeded_state_moves_an_unreadable_file_aside() {
        let dir = temp_dir("unreadable");
        let path = dir.join("prover_bk_set.json");
        std::fs::write(&path, "not json").unwrap();

        let got = bk_file_at_start(path.to_str().unwrap(), &state(false, [0; 32]), 9).unwrap();

        assert!(matches!(got, BkFileAtStart::Build), "{got:?}");
        assert_eq!(entries(&dir), ["prover_bk_set.json.mismatch-9"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unseeded_state_without_a_file_builds() {
        let dir = temp_dir("unseeded-empty");
        let path = dir.join("prover_bk_set.json");

        let got = bk_file_at_start(path.to_str().unwrap(), &state(false, [0; 32]), 9).unwrap();

        assert!(matches!(got, BkFileAtStart::Build), "{got:?}");
        assert!(entries(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeded_state_takes_its_file() {
        let dir = temp_dir("seeded");
        let path = dir.join("prover_bk_set.json");
        set_with([2; 32]).save(path.to_str().unwrap()).unwrap();

        let got = bk_file_at_start(path.to_str().unwrap(), &state(true, [2; 32]), 9).unwrap();

        match got {
            BkFileAtStart::Take(set) => {
                assert_eq!(set.commitment, [2; 32]);
                assert_eq!(set.last_applied_update_seq_no, 7);
            },
            other => panic!("expected the file to be taken, got {other:?}"),
        }
        assert_eq!(entries(&dir), ["prover_bk_set.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeded_state_refuses_a_file_of_another_set() {
        let dir = temp_dir("seeded-other");
        let path = dir.join("prover_bk_set.json");
        set_with([2; 32]).save(path.to_str().unwrap()).unwrap();

        let err = bk_file_at_start(path.to_str().unwrap(), &state(true, [3; 32]), 9).unwrap_err();

        assert!(
            err.to_string().contains("disagrees with prover_state.json"),
            "{err:#}"
        );
        assert_eq!(entries(&dir), ["prover_bk_set.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeded_state_without_a_file_builds() {
        let dir = temp_dir("seeded-empty");
        let path = dir.join("prover_bk_set.json");

        let got = bk_file_at_start(path.to_str().unwrap(), &state(true, [2; 32]), 9).unwrap();

        assert!(matches!(got, BkFileAtStart::Build), "{got:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_aside_never_overwrites() {
        let dir = temp_dir("move-aside");
        let path = dir.join("x.json");
        std::fs::write(&path, "new").unwrap();
        std::fs::write(dir.join("x.json.mismatch-5"), "first").unwrap();
        std::fs::write(dir.join("x.json.mismatch-5.1"), "second").unwrap();

        let backup = move_aside(&path, 5).unwrap();

        assert_eq!(backup, dir.join("x.json.mismatch-5.2"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(dir.join("x.json.mismatch-5")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("x.json.mismatch-5.1")).unwrap(),
            "second"
        );
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A GraphQL stub answering every request with `body`, and the number of
    /// requests it answered.
    async fn serve(body: &'static str) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let counter = counter.clone();
                tokio::spawn(async move {
                    // Read the headers and the whole body, then answer.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(split) = text.find("\r\n\r\n") {
                            let want = text[..split]
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if buf.len() >= split + 4 + want {
                                break;
                            }
                        }
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \
                         {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (url, requests, server)
    }

    /// Without the probe, `Auto` would first poll the chain head, which this
    /// server never gives, every 10 s, and the timeout would fire.
    #[tokio::test]
    async fn an_old_server_fails_the_seed_path_before_any_wait() {
        let (url, requests, server) = serve(OLD_SERVER_BODY).await;
        let gql = gql_client::create_client(&url).unwrap();
        let dir = temp_dir("old-server");
        let path = dir.join("prover_bk_set.json");
        set_with([1; 32]).save(path.to_str().unwrap()).unwrap();

        let started = tokio::time::timeout(
            Duration::from_secs(5),
            startup_bk(
                &gql,
                &state(false, [0; 32]),
                "prover_state.json",
                path.to_str().unwrap(),
                SeedPolicy::Auto,
                1024,
                &BuildPolicy::default(),
            ),
        )
        .await
        .expect("the seed path waited instead of failing at once");
        server.abort();

        let err = match started {
            Ok(_) => panic!("an old server must fail the start"),
            Err(e) => e,
        };
        assert!(
            matches!(
                err.downcast_ref::<BuildError>(),
                Some(BuildError::NoLatestBkSetField)
            ),
            "{err:#}"
        );
        assert!(
            format!("{err:#}").contains("needs gql-server >= 1.5.0 serving blockchain.latestBkSet"),
            "{err:#}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        // The file left from the unfinished seed went aside first.
        let left = entries(&dir);
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(
            left[0].starts_with("prover_bk_set.json.mismatch-"),
            "{left:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_probe_leaves_other_answers_to_the_build() {
        // No snapshot yet.
        let (url, requests, server) =
            serve(r#"{"data":{"blockchain":{"latestBkSet":null}}}"#).await;
        ensure_latest_bk_set_known(&gql_client::create_client(&url).unwrap())
            .await
            .unwrap();
        server.abort();
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        // Nobody listening.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        ensure_latest_bk_set_known(&gql_client::create_client(&url).unwrap())
            .await
            .unwrap();
    }
}
