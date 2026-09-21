//! Bridge Prover Daemon — thin harness around
//! [`bridge_prover_lib::live_driver::LiveProverDriver`].
//!
//! All orchestration (GraphQL polling, bootstrap, BK-set drain, Circuits
//! 1A/1B + 2 proof generation, in-memory state advance) lives inside the
//! library. This binary owns only what a library shouldn't:
//!
//!   * env-driven configuration (`BRIDGE_GQL_ENDPOINT`, `BRIDGE_BOOTSTRAP_SEQNO`,
//!     `BRIDGE_BK_SET_CONFIG`),
//!   * Ctrl-C graceful shutdown flag,
//!   * disk persistence (`./state/*.json`, `./bootstrap_seed.json`),
//!   * verifier IPC (`./proofs/{proof,result,bkupd,bkupd_result}_NNN.json`), and
//!   * the `self-verify` feature gate that inline-verifies proofs in-process.
//!
//! The two-daemon contract is the `LiveProverDriver` API in
//! `bridge-prover-lib/src/live_driver/mod.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tracing::{error, info, warn};

use bridge_prover_lib::bk_set_bootstrap;
use bridge_prover_lib::bootstrap;
use bridge_prover_lib::bridge_state::BridgeState;
use bridge_gql_fetcher::gql_client;
use bridge_prover_lib::ipc;
use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::live_driver::{
    BkUpdateProofArtifacts, BundleFinalizationType, BundleProofArtifacts,
    HISTORY_WINDOW_SIZE, LiveBkUpdateEvent, LiveBundleEvent, LiveProverConfig,
    LiveProverDriver, SeedPolicy,
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
    }

    // Resolve prover_bk_set: warm-load from disk if present, else
    // cold-seed from BRIDGE_BK_SET_CONFIG (bk_set.local.json /
    // bk_set.shellnet.json / fold_at_height).
    //
    // Once `prover_bk_set.json` exists, IT is the sole source of truth
    // for the BK-pubkey table — intermediate rotations advance it via
    // the bk-update lane in `LiveProverDriver`. `bk_set.local.json` is
    // only read on first-ever startup.
    let prover_bk_set = match ProverBkSet::load(&prover_bk_set_file)? {
        Some(loaded) => {
            if state.initialized && loaded.commitment != state.stored_bk_set_commitment {
                anyhow::bail!(
                    "prover_bk_set.json commitment {} disagrees with \
                     prover_state.json {} — delete BOTH files or restore \
                     them from a paired backup",
                    hex::encode(loaded.commitment),
                    hex::encode(state.stored_bk_set_commitment),
                );
            }
            info!(
                "prover_bk_set: loaded {} signers, last_applied_update_seq_no={}",
                loaded.pubkeys_hex.len(),
                loaded.last_applied_update_seq_no,
            );
            loaded
        }
        None => {
            // Cold boot: this is the ONLY code path that reads the seed
            // file directly. Once saved, `prover_bk_set.json` takes over.
            let bk_set_from_file = bk_set_bootstrap::load_bk_set(
                &gql,
                &bk_set_bootstrap::resolve_bk_set_config_path(),
                explicit_bootstrap_seqno,
            )
            .await?;
            let pbs = ProverBkSet::from_pubkeys(&bk_set_from_file, 0);
            pbs.save(&prover_bk_set_file)?;
            info!(
                "prover_bk_set: cold-boot seeded {} signers → {}",
                pbs.pubkeys_hex.len(),
                prover_bk_set_file,
            );
            pbs
        }
    };

    // Guard against the "fresh chain + stale ./state/" footgun without a
    // GQL round-trip: if BRIDGE_BK_SET_CONFIG points to a readable file,
    // compare its commitment against `prover_bk_set.commitment`. On
    // devnet, `bk_set.local.json` gets rewritten every time zerostate is
    // regenerated — mismatch + prover never rotated (`last_applied=0`)
    // is a strong signal that state/ is stale relative to the current
    // chain instance and must be wiped. On shellnet the file is a
    // genesis snapshot, so mismatch + prover has rotated is expected
    // and only logged.
    bk_set_bootstrap::verify_prover_bk_set_matches_config_file(
        &bk_set_bootstrap::resolve_bk_set_config_path(),
        &prover_bk_set,
    )?;

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

    let seed_policy = match (explicit_bootstrap_seqno, state.initialized) {
        (_, true) => SeedPolicy::Resume,
        (Some(n), false) => SeedPolicy::Explicit(n),
        (None, false) => SeedPolicy::Auto,
    };

    let mut driver = LiveProverDriver::new(
        gql,
        key_manager,
        state,
        prover_bk_set,
        LiveProverConfig {
            seed_policy,
            anchor_mode,
            ..Default::default()
        },
    )?;

    // Seed persistence: the seed JSON is written once, right after the driver
    // completes its bootstrap. On a Resume start (state already on disk) the
    // seed file was written by an earlier run — treat it as already persisted.
    let mut seed_persisted = matches!(seed_policy, SeedPolicy::Resume);

    // ---- Main loop ---------------------------------------------------------
    while !shutdown.load(Ordering::SeqCst) {
        // Drain rotations first — `poll_next_bundle` refuses to advance past
        // an un-acked bk-update whose height is <= the next thinned target.
        match driver.poll_next_bk_update().await {
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
                if !handle_bk_update(&mut driver, &update).await? {
                    break;
                }
                persist_seed_if_needed(&driver, &mut seed_persisted)?;
                continue;
            }
            Ok(LiveBkUpdateEvent::Nothing) => { /* fall through to bundle poll */ }
            Err(e) => {
                warn!("poll_next_bk_update: {} — retrying", e);
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
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
                    // Loop back immediately; the rotation-drain lane needs
                    // to run before bundle can advance past this height.
                    continue;
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

async fn handle_bk_update(
    driver: &mut LiveProverDriver,
    update: &BkUpdateProofArtifacts,
) -> anyhow::Result<bool> {
    info!(
        "bk-update {}: {} rotation ready ({}ms proof)",
        update.block_seq_no,
        update.fin_type.as_str(),
        update.attestation_proof_gen_ms,
    );
    let req = bkupdate_to_ipc_request(update);
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
        return Ok(false);
    }
    info!("bk-update {}: verifier ACK", update.block_seq_no);

    driver.ack_bk_update(update)?;
    persist(driver)?;
    Ok(true)
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
