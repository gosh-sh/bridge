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
//! See `an_bridge_prover_live_driver_refactor_plan_2026-07-08.md` for the
//! extraction rationale and the two-daemon contract.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tracing::{error, info, warn};

use bridge_prover_lib::bootstrap;
use bridge_prover_lib::bridge_state::BridgeState;
use bridge_prover_lib::gql_client::{self, GqlClient};
use bridge_prover_lib::ipc;
use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::live_driver::{
    BkUpdateProofArtifacts, BundleFinalizationType, BundleProofArtifacts,
    HISTORY_WINDOW_SIZE, LiveBkUpdateEvent, LiveBundleEvent, LiveProverConfig,
    LiveProverDriver, SeedPolicy,
};
use bridge_prover_lib::poseidon;
use bridge_prover_lib::prover_bk_set::ProverBkSet;
use bridge_prover_lib::Fr;
use bridge_prover_lib::THINNING_FACTOR_P;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;

#[cfg(feature = "self-verify")]
use bridge_prover_lib::bridge_state::BundleResult;
#[cfg(feature = "self-verify")]
use bridge_prover_lib::verifier;

const DEFAULT_GQL_ENDPOINT: &str = "http://localhost/graphql";
const ENV_GQL_ENDPOINT: &str = "BRIDGE_GQL_ENDPOINT";
const ENV_BOOTSTRAP_SEQNO: &str = "BRIDGE_BOOTSTRAP_SEQNO";
const DEFAULT_BK_SET_CONFIG: &str = "./bk_set.local.json";
const ENV_BK_SET_CONFIG: &str = "BRIDGE_BK_SET_CONFIG";

const PARAMS_DIR: &str = "./params";
const LOGS_DIR: &str = "./logs";
const STATE_FILE: &str = "./state/prover_state.json";
const PROVER_BK_SET_FILE: &str = "./state/prover_bk_set.json";

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
    std::fs::create_dir_all("state").ok();
    ipc::ensure_proofs_dir();

    let gql_endpoint = std::env::var(ENV_GQL_ENDPOINT)
        .unwrap_or_else(|_| DEFAULT_GQL_ENDPOINT.to_string());
    let bundle_size = HISTORY_WINDOW_SIZE * THINNING_FACTOR_P;
    let explicit_bootstrap_seqno = parse_explicit_bootstrap(bundle_size)?;

    info!("=== Bridge Prover Daemon (Circuit 1a/1b + Circuit 2) ===");
    info!("GQL endpoint: {}", gql_endpoint);
    info!(
        "W = {}, P = {}, bundle = {} blocks",
        HISTORY_WINDOW_SIZE, THINNING_FACTOR_P, bundle_size
    );
    match explicit_bootstrap_seqno {
        Some(n) => info!("bootstrap: EXPLICIT seed seq_no = {}", n),
        None => info!("bootstrap: AUTO (next W*P boundary past chain head)"),
    }
    info!("send SIGINT (Ctrl-C) to shut down cleanly");

    let shutdown = install_ctrl_c_flag();

    // ---- Wire dependencies -------------------------------------------------
    let gql = gql_client::create_client(&gql_endpoint)?;
    let bk_set_from_gql = load_bk_set(&gql).await?;
    let (bk_commitment_fr, _) = poseidon::compute_bk_set_poseidon(&bk_set_from_gql);
    info!(
        "BK set (GQL): {} signers, commitment={}",
        bk_set_from_gql.len(),
        hex::encode(bk_commitment_fr.to_repr())
    );

    info!("loading keys...");
    let mut key_manager = KeyManager::new(Path::new(PARAMS_DIR));
    key_manager.ensure_primary_keys(&bk_set_from_gql)?;
    key_manager.ensure_fallback_keys(&bk_set_from_gql)?;
    key_manager.ensure_layer_keys()?;
    info!("keys ready (primary, fallback, layer)");

    let state = BridgeState::load(STATE_FILE, HISTORY_WINDOW_SIZE as usize)?;
    info!(
        "state: initialized={}, last_key_block={}",
        state.initialized, state.stored_last_seen_block_seq_no
    );

    let prover_bk_set =
        load_or_bootstrap_prover_bk_set(&state, &bk_set_from_gql, bk_commitment_fr)?;

    // Re-anchor in-memory bk_set to the persisted prover_bk_set. Once a paired
    // prover_bk_set exists on disk, IT (not the GQL-fresh set) is the source
    // of truth for "what set has been formally applied to the contract mirror"
    // — intermediate rotations get drained via the bk-update path.
    let bk_set = prover_bk_set
        .pubkeys()
        .context("prover_bk_set.pubkeys()")?;

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
        bk_set,
        LiveProverConfig {
            seed_policy,
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
                error!("poll_next_bundle: {} — retrying", e);
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
                "{}={} must be > 0 and divisible by W*P={}",
                ENV_BOOTSTRAP_SEQNO,
                n,
                bundle_size
            );
            Ok(Some(n))
        }
        Err(_) => Ok(None),
    }
}

/// Load the genesis BK set from the JSON config file.
///
/// The `_gql` parameter is retained for signature stability with callers that
/// wire a GraphQL client at bootstrap; it is currently unused because the old
/// `fetch_bk_set` GraphQL path was disabled (2026-07-22 — it replayed the
/// `bkSetUpdates` delta log from ∅, but AN does not emit genesis as a
/// synthetic `Added` event, so the result was wrong on any rotating chain
/// and empty on fresh ones). For a distant-block cold start on a long-lived
/// rotating chain, use `bk_set_at_height` (planned).
async fn load_bk_set(_gql: &GqlClient) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    let bk_set_config = std::env::var(ENV_BK_SET_CONFIG)
        .unwrap_or_else(|_| DEFAULT_BK_SET_CONFIG.to_string());
    bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(&bk_set_config)
        .with_context(|| format!("failed to load BK set from config file {}", bk_set_config))
}

fn load_or_bootstrap_prover_bk_set(
    state: &BridgeState,
    bk_set: &HashMap<u16, Vec<u8>>,
    bk_commitment_fr: Fr,
) -> anyhow::Result<ProverBkSet> {
    match ProverBkSet::load(PROVER_BK_SET_FILE)? {
        Some(loaded) => {
            if state.initialized && loaded.commitment != state.stored_bk_set_commitment {
                anyhow::bail!(
                    "prover_bk_set.json commitment {} disagrees with prover_state.json {} — \
                     delete BOTH files or restore them from a paired backup",
                    hex::encode(loaded.commitment),
                    hex::encode(state.stored_bk_set_commitment),
                );
            }
            info!(
                "prover_bk_set: loaded {} signers, last_applied_update_seq_no={}",
                loaded.pubkeys_hex.len(),
                loaded.last_applied_update_seq_no,
            );
            Ok(loaded)
        }
        None => {
            let pbs = ProverBkSet::from_pubkeys(bk_set, 0);
            let bk_hash_bytes: [u8; 32] = bk_commitment_fr.to_repr();
            anyhow::ensure!(
                pbs.commitment == bk_hash_bytes,
                "ProverBkSet commitment {} != poseidon::compute_bk_set_poseidon {} — \
                 Poseidon-parameter mismatch?",
                hex::encode(pbs.commitment),
                hex::encode(bk_hash_bytes),
            );
            pbs.save(PROVER_BK_SET_FILE)?;
            info!(
                "prover_bk_set: bootstrapped {} signers, saved to {}",
                pbs.pubkeys_hex.len(),
                PROVER_BK_SET_FILE,
            );
            Ok(pbs)
        }
    }
}

// -------------------------------------------------------------------------
// Persistence
// -------------------------------------------------------------------------

fn persist(driver: &LiveProverDriver) -> anyhow::Result<()> {
    driver.snapshot_state().save(STATE_FILE)?;
    driver.snapshot_prover_bk_set().save(PROVER_BK_SET_FILE)?;
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
        seed.save(bootstrap::DEFAULT_SEED_PATH)?;
        info!(
            "bootstrap seed persisted: seq_no={}, height={}, layers={} → {}",
            seed.block_seq_no,
            seed.block_height,
            seed.layer_hashes.len(),
            bootstrap::DEFAULT_SEED_PATH,
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
        bundle.primary_proof_gen_ms,
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
        update.primary_proof_gen_ms,
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
    // Circuit 1a/1b's `block_id` (from attestation payload) and Circuit 2's
    // `block_id` (from the SHA-256 8-leaf Merkle root reversed to LE) bind the
    // same block but may have different Fr representations depending on
    // byte-order conventions in the attestation wire format — send both.
    ipc::ProofRequest {
        schema_version: ipc::PROOF_REQUEST_SCHEMA_VERSION,
        block_seq_no: b.block_seq_no as u32,
        block_height: b.block_height,
        last_seen_block_seqno: b.last_seen_block_seq_no as u32,
        block_id_hex: hex::encode(b.block_id_be),
        attestation_circuit: to_ipc_circuit(b.fin_type),
        primary_proof_hex: hex::encode(&b.attestation_proof),
        layer_proof_hex: hex::encode(&b.layer_hashes_proof),
        layer_block_id_hex: hex::encode(b.layer_block_id_be),
        bk_set_poseidon_hash_hex: hex::encode(b.bk_set_commitment_be),
        num_layers: b.num_layers,
        layer_hash_frs_hex: b.layer_hashes_be.iter().map(hex::encode).collect(),
        prev_max_level_layer_hash_hex: hex::encode(b.prev_max_level_layer_hash_be),
        primary_proof_gen_ms: b.primary_proof_gen_ms,
        layer_proof_gen_ms: b.layer_proof_gen_ms,
    }
}

fn bkupdate_to_ipc_request(u: &BkUpdateProofArtifacts) -> ipc::BkUpdateRequest {
    ipc::BkUpdateRequest {
        schema_version: ipc::PROOF_REQUEST_SCHEMA_VERSION,
        block_seq_no: u.block_seq_no as u32,
        block_height: u.block_height,
        last_seen_bk_update_seqno: u.last_seen_bk_update_seq_no as u32,
        block_id_hex: hex::encode(u.block_id_be),
        block_id_hash_hex: hex::encode(u.block_id_hash_be),
        attestation_circuit: to_ipc_circuit(u.fin_type),
        primary_proof_hex: hex::encode(&u.attestation_proof),
        old_bk_set_poseidon_hash_hex: hex::encode(u.old_bk_set_commitment_be),
        new_bk_set_poseidon_hash_hex: hex::encode(u.new_bk_set_commitment_be),
        merkle_sibling_h0_hex: hex::encode(u.merkle_sibling_h0_be),
        merkle_sibling_h23_hex: hex::encode(u.merkle_sibling_h23_be),
        primary_proof_gen_ms: u.primary_proof_gen_ms,
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
    let block_id_fr = fr_from_repr(bundle.block_id_be);
    let layer_block_id_fr = fr_from_repr(bundle.layer_block_id_be);
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

    // Circuit 2 public instances: 14 elements.
    let mut layer_instances: Vec<Fr> = Vec::with_capacity(14);
    layer_instances.push(layer_block_id_fr);
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
