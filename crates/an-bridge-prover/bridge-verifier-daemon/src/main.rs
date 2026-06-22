//! Bridge Verifier Daemon — verifies Circuit 1a + Circuit 2 proofs.
//!
//! Watches the proofs/ directory for combined proof files from the prover daemon.
//! Verifies both proofs, cross-references public instances, updates state.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

use bridge_prover_lib::bootstrap::{self, BootstrapSeed};
use bridge_prover_lib::bridge_state::{BridgeState, MAX_LAYERS};
use bridge_event_prover_lib as event_verifier;
use bridge_prover_lib::ipc;
use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::poseidon;
use bridge_prover_lib::verifier;
use bridge_prover_lib::Fr;

use serde::{Deserialize, Serialize};

use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;

const PARAMS_DIR: &str = "./params";
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often to log a heartbeat summary while the loop is running.
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(60);
const STATE_FILE: &str = "./state/verifier_state.json";
/// Default GraphQL endpoint when `BRIDGE_GQL_ENDPOINT` is not set. Same env
/// var the prover daemon honours, so a single export in the shell selects the
/// network for both daemons. Used only by [`load_bk_set_commitment`] — the
/// verifier has no other GQL traffic; bk_set.json is the fallback if GQL is
/// unreachable.
const DEFAULT_GQL_ENDPOINT: &str = "http://localhost/graphql";
const ENV_GQL_ENDPOINT: &str = "BRIDGE_GQL_ENDPOINT";
const BK_SET_CONFIG: &str = "./bk_set.json";

// History window size — must match the prover daemon and the node. Sourced
// from the vendored poseidon_dense constant so it can never drift.
const HISTORY_WINDOW_SIZE: usize =
    bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE;

#[derive(Default)]
struct Stats {
    total_proofs: u32,
    both_verified_ok: u32,
    primary_only_ok: u32,
    layer_only_ok: u32,
    both_failed: u32,
    failures: Vec<(u32, String)>,
    // Event-proof (Circuit 4) counters. Independent seqno space from
    // primary/layer above (`bridge-event-prove` writes
    // `proof_event_NNNNNN.json` separately from `proof_NNNNNN.json`).
    event_total: u32,
    event_verified_ok: u32,
    event_anchor_mismatch: u32,
    event_proof_invalid: u32,
    event_failures: Vec<(u32, String)>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    std::fs::create_dir_all("state").ok();
    ipc::ensure_proofs_dir();

    let gql_endpoint = std::env::var(ENV_GQL_ENDPOINT)
        .unwrap_or_else(|_| DEFAULT_GQL_ENDPOINT.to_string());

    info!("=== Bridge Verifier Daemon (Circuit 1a + Circuit 2) ===");
    info!("GQL endpoint: {} (BK-set fetch only; falls back to {})", gql_endpoint, BK_SET_CONFIG);
    info!("running indefinitely; send SIGINT (Ctrl-C) to shut down cleanly");

    // Graceful-shutdown flag flipped by the Ctrl-C handler. Checked at the top
    // of each loop iteration so we never tear down mid-verify.
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let s = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("Ctrl-C received, shutting down at next safe point...");
                s.store(true, Ordering::SeqCst);
            }
        });
    }

    // 1. Load BK set commitment (for Circuit 1a verification reference).
    let bk_set_commitment = load_bk_set_commitment(&gql_endpoint).await?;
    info!("BK set commitment: {:?}", bk_set_commitment);

    // 2. Load key manager (SRS + VKs only, no PKs needed).
    info!("loading SRS and VKs...");
    let key_manager = KeyManager::new(Path::new(PARAMS_DIR));
    if key_manager.primary_vk.is_none() {
        anyhow::bail!(
            "primary VK not found in {}. Run the prover first to generate keys.",
            PARAMS_DIR
        );
    }
    if key_manager.fallback_vk.is_none() {
        anyhow::bail!(
            "fallback VK not found in {}. Run the prover first to generate keys \
             (the fallback VK is materialised alongside the primary VK on cold start).",
            PARAMS_DIR
        );
    }
    if key_manager.layer_vk.is_none() {
        anyhow::bail!(
            "layer VK not found in {}. Run the prover first to generate keys.",
            PARAMS_DIR
        );
    }
    if key_manager.event_vk.is_none() {
        anyhow::bail!(
            "event VK not found in {}. Run the event prover (Circuit 4) first to generate keys.",
            PARAMS_DIR
        );
    }
    info!("VKs loaded (primary + fallback + layer + event)");

    // 3. Load state.
    let mut state = BridgeState::load(STATE_FILE, HISTORY_WINDOW_SIZE)?;
    info!(
        "state loaded: initialized={}, last_key_block={}",
        state.initialized, state.stored_last_seen_block_seq_no
    );

    // 3a. Cold-start bootstrap from the seed file written by the prover.
    //
    //     This mirrors the on-chain contract receiving its genesis
    //     `GlobalHistoryData` via constructor arguments at deployment:
    //     the prover-as-deployer produces `state/bootstrap_seed.json`, and
    //     the verifier-as-contract consumes it exactly once. Without this,
    //     the L1 window's first key block (block 8 on `W=8`) was present
    //     in the prover state but absent from the verifier — a one-entry
    //     drift from genesis onward.
    if !state.initialized {
        match BootstrapSeed::load(bootstrap::DEFAULT_SEED_PATH)? {
            Some(seed) => {
                info!(
                    "loading bootstrap seed from {}: seqno={}, height={}, layers={}",
                    bootstrap::DEFAULT_SEED_PATH,
                    seed.block_seq_no,
                    seed.block_height,
                    seed.layer_hashes.len(),
                );
                seed.apply(&mut state);
                state.save(STATE_FILE)?;
                info!(
                    "initialized from seed: seqno={}, height={}",
                    state.stored_last_seen_block_seq_no, state.stored_last_seen_block_height,
                );
            }
            None => {
                info!(
                    "no bootstrap seed at {} yet — waiting for prover to write it",
                    bootstrap::DEFAULT_SEED_PATH,
                );
            }
        }
    }

    // 4. Watch for proof files and verify.
    let mut last_seen_seqno: u32 = state.stored_last_seen_block_seq_no as u32;
    // Bk-set-update cursor. Mirrors what the future ETH contract will track
    // as `storedLastBkUpdateSeq`. Starts from the persisted v4 field so a
    // restart resumes from the last verified bk-update without re-applying.
    let mut last_seen_bk_update_seqno: u32 = state.stored_last_bk_set_update_seq_no as u32;
    // Event-proof seqno tracker. Independent from `last_seen_seqno` because
    // `bridge-event-prove` writes `proof_event_NNNNNN.json` with its own
    // counter (typically 0..N per orchestrator run). Not persisted across
    // restarts — on restart we re-scan from 0, which means any leftover
    // `*.result.json` files get rewritten. That is intentional: the daemon
    // is the source of truth, and re-verification is cheap.
    let mut last_seen_event_seqno: i64 = -1;
    let mut bootstrapped = state.initialized;
    let mut stats = Stats::default();
    let t_total = Instant::now();
    let mut last_stats_log = Instant::now();

    info!("watching proofs/ directory for incoming proofs (block bundles + event proofs)...");

    loop {
        if shutdown.load(Ordering::SeqCst) {
            info!("shutdown flag set, exiting main loop");
            break;
        }
        if last_stats_log.elapsed() >= STATS_LOG_INTERVAL {
            info!(
                "[heartbeat] bundles: total={}, both_ok={}, primary_only={}, layer_only={}, both_failed={} | events: total={}, ok={}, anchor_miss={}, invalid={} | uptime={:?}",
                stats.total_proofs,
                stats.both_verified_ok,
                stats.primary_only_ok,
                stats.layer_only_ok,
                stats.both_failed,
                stats.event_total,
                stats.event_verified_ok,
                stats.event_anchor_mismatch,
                stats.event_proof_invalid,
                t_total.elapsed()
            );
            last_stats_log = Instant::now();
        }

        // If not bootstrapped, retry loading the seed file. The prover writes
        // `state/bootstrap_seed.json` on its own cold start; if the verifier
        // was started first, this is the loop point at which it picks it up.
        if !bootstrapped {
            match BootstrapSeed::load(bootstrap::DEFAULT_SEED_PATH)? {
                Some(seed) => {
                    info!(
                        "bootstrapping from seed at {}: seqno={}, height={}, layers={}",
                        bootstrap::DEFAULT_SEED_PATH,
                        seed.block_seq_no,
                        seed.block_height,
                        seed.layer_hashes.len(),
                    );
                    seed.apply(&mut state);
                    state.save(STATE_FILE)?;
                    last_seen_seqno = state.stored_last_seen_block_seq_no as u32;
                    bootstrapped = true;
                }
                None => {
                    // Seed file not yet written. Stay idle and try again on the
                    // next tick — sleep below covers the wait.
                }
            }
        }

        // Bk-set-update bundles take priority over layer bundles. The
        // prover writes them with `seq_no` set to the bk-update block's
        // seq_no; if a layer bundle also targets the same key block (case
        // B in `docs/bk_set_update_no_circuit3_plan.md` §8), the contract
        // must rotate the commitment BEFORE the next layer bundle arrives
        // — but the layer bundle for the bk-update block itself uses the
        // OLD commitment and so still verifies. Processing bk-update
        // bundles first preserves that order without extra coordination.
        if let Some(bkupd_seqno) = find_next_bk_update_file(last_seen_bk_update_seqno) {
            process_bk_update_bundle(
                bkupd_seqno,
                &key_manager,
                &mut state,
                &mut last_seen_bk_update_seqno,
                STATE_FILE,
            );
            continue;
        }

        // Look for proofs with seq_no > last_seen_seqno.
        // Since key blocks may not be consecutive, scan for any proof file.
        let next_proof = find_next_proof_file(last_seen_seqno);

        if let Some(next_seqno) = next_proof {
            info!("found proof for key block {}", next_seqno);

            let request = match ipc::read_proof_request(next_seqno) {
                Ok(r) => r,
                Err(e) => {
                    error!("block {}: failed to read proof file: {}", next_seqno, e);
                    write_failure(next_seqno, &format!("read error: {}", e));
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    stats.failures.push((next_seqno, e.to_string()));
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            // Validate consistency.
            if request.last_seen_block_seqno != last_seen_seqno {
                let msg = format!(
                    "last_seen mismatch: prover says {} but verifier tracked {}",
                    request.last_seen_block_seqno, last_seen_seqno
                );
                warn!("block {}: {} (proceeding anyway for PoC)", next_seqno, msg);
            }

            // ---- Verify Circuit 1a ----
            let block_id_fr = match ipc::fr_from_hex(&request.block_id_hex) {
                Ok(fr) => fr,
                Err(e) => {
                    let msg = format!("invalid block_id_hex: {}", e);
                    error!("block {}: {}", next_seqno, msg);
                    write_failure(next_seqno, &msg);
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            let bk_set_hash_fr = match ipc::fr_from_hex(&request.bk_set_poseidon_hash_hex) {
                Ok(fr) => fr,
                Err(e) => {
                    let msg = format!("invalid bk_set_poseidon_hash_hex: {}", e);
                    error!("block {}: {}", next_seqno, msg);
                    write_failure(next_seqno, &msg);
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            let primary_proof_bytes = match hex::decode(&request.primary_proof_hex) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("invalid primary_proof_hex: {}", e);
                    error!("block {}: {}", next_seqno, msg);
                    write_failure(next_seqno, &msg);
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            let block_seq_no_fr = Fr::from(request.block_seq_no as u64);
            let last_seen_fr = Fr::from(request.last_seen_block_seqno as u64);

            let primary_instances = vec![
                block_id_fr,
                bk_set_hash_fr,
                block_seq_no_fr,
                last_seen_fr,
            ];

            // Pick the verifying key based on which attestation circuit the
            // prover ran. The 4-public-instance layout is identical for 1a
            // and 1b, so only the VK varies; everything else (transcript,
            // SHPLONK, single-strategy) is shared.
            let t = Instant::now();
            let (circuit_label, primary_verified) = match request.attestation_circuit {
                ipc::AttestationCircuit::Primary => (
                    "Circuit 1a (Primary)",
                    verifier::verify_primary_proof(
                        &key_manager,
                        &primary_proof_bytes,
                        &primary_instances,
                    ),
                ),
                ipc::AttestationCircuit::Fallback => (
                    "Circuit 1b (Fallback)",
                    verifier::verify_fallback_proof(
                        &key_manager,
                        &primary_proof_bytes,
                        &primary_instances,
                    ),
                ),
            };
            let primary_time = t.elapsed();
            info!(
                "block {}: {} {} ({:?})",
                next_seqno,
                circuit_label,
                if primary_verified { "VERIFIED" } else { "FAILED" },
                primary_time
            );

            // ---- Verify Circuit 2 ----
            let layer_proof_bytes = match hex::decode(&request.layer_proof_hex) {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("invalid layer_proof_hex: {}", e);
                    error!("block {}: {}", next_seqno, msg);
                    write_failure(next_seqno, &msg);
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            let prev_hash_fr = match ipc::fr_from_hex(&request.prev_max_level_layer_hash_hex) {
                Ok(fr) => fr,
                Err(e) => {
                    let msg = format!("invalid prev_max_level_layer_hash_hex: {}", e);
                    error!("block {}: {}", next_seqno, msg);
                    write_failure(next_seqno, &msg);
                    stats.total_proofs += 1;
                    stats.both_failed += 1;
                    last_seen_seqno = next_seqno;
                    continue;
                }
            };

            // Build Circuit 2 public instances (14 values).
            let mut layer_instances = Vec::with_capacity(14);
            // Circuit 2 computes its own block_id from the Merkle path.
            let layer_block_id_hex = request.layer_block_id_hex.as_str();
            let layer_block_id_fr = match ipc::fr_from_hex(layer_block_id_hex) {
                Ok(fr) => fr,
                Err(_) => {
                    // Fallback for old proof files without layer_block_id_hex.
                    ipc::fr_from_hex(&request.block_id_hex).unwrap_or(Fr::zero())
                }
            };
            layer_instances.push(layer_block_id_fr);     // [0] block_id
            layer_instances.push(bk_set_hash_fr);        // [1] bk_set_poseidon_hash
            layer_instances.push(Fr::from(request.num_layers as u64)); // [2] num_layers
            for hex_str in &request.layer_hash_frs_hex {
                let fr = ipc::fr_from_hex(hex_str).unwrap_or(Fr::zero());
                layer_instances.push(fr);                // [3..12]
            }
            // Pad to 10 layer hashes if needed.
            while layer_instances.len() < 13 {
                layer_instances.push(Fr::zero());
            }
            layer_instances.push(prev_hash_fr);          // [13]

            let t = Instant::now();
            let layer_verified = verifier::verify_layer_proof(
                &key_manager,
                &layer_proof_bytes,
                &layer_instances,
            );
            let layer_time = t.elapsed();
            info!(
                "block {}: Circuit 2 {} ({:?})",
                next_seqno,
                if layer_verified { "VERIFIED" } else { "FAILED" },
                layer_time
            );

            // Record results.
            stats.total_proofs += 1;
            let result = ipc::VerifyResult {
                block_seq_no: next_seqno,
                primary_verified,
                layer_verified,
                error: if primary_verified && layer_verified {
                    None
                } else {
                    Some(format!(
                        "primary={}, layer={}",
                        primary_verified, layer_verified
                    ))
                },
            };
            ipc::write_result(&result)?;

            if primary_verified && layer_verified {
                stats.both_verified_ok += 1;
                info!("block {}: BOTH VERIFIED OK", next_seqno);

                // ---- Tightened append-bundle semantics ----
                // 1. Refuse to rewind: only append when this block is strictly
                //    newer than what's already mirrored. The contract enforces
                //    the same monotonicity; the verifier daemon mirrors it.
                let next_seq_u64 = next_seqno as u64;
                if state.initialized
                    && next_seq_u64 <= state.stored_last_seen_block_seq_no
                {
                    warn!(
                        "block {}: refusing to append non-monotone bundle \
                         (stored_last_seen={}); state left unchanged",
                        next_seqno, state.stored_last_seen_block_seq_no
                    );
                } else {
                    // 2. Pull only the first `num_layers` slots from the
                    //    proof request and drop any all-zero entries — those
                    //    represent layers the prover left unset.
                    let new_layer_hashes: Vec<([u8; 32], u8)> = request.layer_hash_frs_hex
                        .iter()
                        .take(request.num_layers as usize)
                        .enumerate()
                        .filter_map(|(i, hex_str)| {
                            let fr = ipc::fr_from_hex(hex_str).unwrap_or(Fr::zero());
                            let bytes: [u8; 32] = fr.to_repr();
                            if bytes == [0u8; 32] {
                                None
                            } else {
                                Some((bytes, (i + 1) as u8))
                            }
                        })
                        .collect();
                    let bk_hash_bytes: [u8; 32] = bk_set_hash_fr.to_repr();
                    // `block_height` is the thread-anchored height from the
                    // node's envelope (carried in `ProofRequest` v2). In
                    // multi-thread Acki Nacki this resets across thread
                    // crossings, so it is NOT the same as `block_seq_no` —
                    // mirroring it explicitly is what keeps `heights[W]`
                    // aligned with the contract's per-layer rolling window.
                    state.append_bundle(
                        &new_layer_hashes,
                        request.block_height,
                        next_seq_u64,
                        bk_hash_bytes,
                    );
                    state.save(STATE_FILE)?;
                }
                // block_id_fr is informational only in v2 state — no longer
                // stored (the contract mirror tracks per-layer rolling windows,
                // not the latest block_id).
                let _ = block_id_fr;
            } else if primary_verified {
                stats.primary_only_ok += 1;
            } else if layer_verified {
                stats.layer_only_ok += 1;
            } else {
                stats.both_failed += 1;
                stats.failures.push((next_seqno, "both circuits failed".to_string()));
            }

            last_seen_seqno = next_seqno;
        } else if let Some(next_event_seqno) =
            find_next_event_proof_file(last_seen_event_seqno)
        {
            info!("found event proof seq_no={}", next_event_seqno);
            process_event_proof(next_event_seqno, &key_manager, &state, &mut stats);
            last_seen_event_seqno = next_event_seqno as i64;
        } else {
            // No new proof of either kind — keep polling. Idle-shutdown was
            // removed; the verifier now mirrors the contract and runs
            // forever until SIGINT.
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // Print summary.
    let elapsed = t_total.elapsed();
    info!("\n=== VERIFIER SUMMARY ===");
    info!("total time:             {:?}", elapsed);
    info!("total proofs received:  {}", stats.total_proofs);
    info!("both verified OK:       {}", stats.both_verified_ok);
    info!("primary only OK:        {}", stats.primary_only_ok);
    info!("layer only OK:          {}", stats.layer_only_ok);
    info!("both failed:            {}", stats.both_failed);
    if !stats.failures.is_empty() {
        info!("failures:");
        for (seq_no, err) in &stats.failures {
            info!("  block {}: {}", seq_no, err);
        }
    }
    info!("");
    info!("event proofs received:  {}", stats.event_total);
    info!("event verified OK:      {}", stats.event_verified_ok);
    info!("event anchor mismatch:  {}", stats.event_anchor_mismatch);
    info!("event proof invalid:    {}", stats.event_proof_invalid);
    if !stats.event_failures.is_empty() {
        info!("event failures:");
        for (seq_no, err) in &stats.event_failures {
            info!("  event {}: {}", seq_no, err);
        }
    }

    Ok(())
}

async fn load_bk_set_commitment(gql_endpoint: &str) -> anyhow::Result<Fr> {
    let bk_set = match bridge_prover_lib::gql_client::create_client(gql_endpoint) {
        Ok(gql) => match bridge_prover_lib::bk_set_fetcher::fetch_bk_set(&gql).await {
            Ok(bk) => {
                info!("BK set loaded from GraphQL: {} signers", bk.len());
                bk
            }
            Err(e) => {
                info!("GraphQL BK set failed ({}), trying config file", e);
                bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(BK_SET_CONFIG)?
            }
        },
        Err(_) => bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(BK_SET_CONFIG)?,
    };
    Ok(poseidon::compute_bk_set_poseidon(&bk_set).0)
}

/// Scan proofs/ for any proof file with seq_no > last_seen.
fn find_next_proof_file(last_seen: u32) -> Option<u32> {
    let dir = match std::fs::read_dir("proofs") {
        Ok(d) => d,
        Err(_) => return None,
    };
    let mut candidates: Vec<u32> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("proof_") && name.ends_with(".json") {
                let num_str = name.trim_start_matches("proof_").trim_end_matches(".json");
                num_str.parse::<u32>().ok()
            } else {
                None
            }
        })
        .filter(|&seq| seq > last_seen)
        .collect();
    candidates.sort();
    candidates.first().copied()
}

fn write_failure(seq_no: u32, error: &str) {
    let result = ipc::VerifyResult {
        block_seq_no: seq_no,
        primary_verified: false,
        layer_verified: false,
        error: Some(error.to_string()),
    };
    if let Err(e) = ipc::write_result(&result) {
        error!("failed to write result for block {}: {}", seq_no, e);
    }
}

// =====================================================================
// BK-set-update bundle scanner + verifier (schema v4)
// =====================================================================
//
// On local devnet the prover never produces `bkupd_*.json` (the BK set is
// fixed at genesis), so the scanner returns `None` every poll and this
// path stays dormant. On shellnet — once Phase 3 lands the prover side —
// the scanner picks up bk-update bundles as they appear and applies them.
//
// The three checks performed here are exactly the three checks the future
// Solidity `applyBkSetUpdate` will perform:
//   1. ZK attestation against `[block_id, L2, block_seq_no, last_seen]`.
//   2. SHA-256 Merkle binding: `root(L2, L3, H0, H23) == block_id`.
//   3. Monotonicity: `block_seq_no > stored_last_bk_set_update_seq_no`.
// The verifier never sees the pubkey list — by design (the ETH contract
// will not store it, so the daemon must not either).

/// Scan proofs/ for any `bkupd_NNNNNN.json` with seq_no > last_seen.
fn find_next_bk_update_file(last_seen: u32) -> Option<u32> {
    let dir = match std::fs::read_dir("proofs") {
        Ok(d) => d,
        Err(_) => return None,
    };
    let mut candidates: Vec<u32> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Must distinguish `bkupd_NNNNNN.json` from `bkupd_result_NNNNNN.json`.
            if name.starts_with("bkupd_")
                && name.ends_with(".json")
                && !name.starts_with("bkupd_result_")
            {
                let num_str = name
                    .trim_start_matches("bkupd_")
                    .trim_end_matches(".json");
                num_str.parse::<u32>().ok()
            } else {
                None
            }
        })
        .filter(|&seq| seq > last_seen)
        .collect();
    candidates.sort();
    candidates.first().copied()
}

fn write_bk_update_failure(seq_no: u32, error: &str) {
    let r = ipc::BkUpdateResult {
        block_seq_no: seq_no,
        attestation_verified: false,
        merkle_verified: false,
        monotonicity_ok: false,
        verify_ok: false,
        error: Some(error.to_string()),
    };
    if let Err(e) = ipc::write_bk_update_result(&r) {
        error!("failed to write bk-update result for block {}: {}", seq_no, e);
    }
}

/// SHA-256(left ‖ right) for Merkle internal nodes.
fn sha256_concat(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Verify and (on success) apply one bk-update bundle. Mirrors the future
/// Solidity entry point shape — three checks, all-or-nothing apply, state
/// touches only the commitment + the bk-update cursor.
fn process_bk_update_bundle(
    seq_no: u32,
    key_manager: &KeyManager,
    state: &mut BridgeState,
    last_seen_bk_update_seqno: &mut u32,
    state_file: &str,
) {
    info!("found bk-update bundle for seq_no={}", seq_no);

    let req = match ipc::read_bk_update_request(seq_no) {
        Ok(r) => r,
        Err(e) => {
            error!("bk-update {}: failed to read file: {}", seq_no, e);
            write_bk_update_failure(seq_no, &format!("read error: {}", e));
            *last_seen_bk_update_seqno = seq_no;
            return;
        }
    };

    // Decode the open payload first — it's cheap and the result file is
    // most useful when the parse failure pinpoints which field was malformed.
    let l2 = match decode_hash32(&req.old_bk_set_poseidon_hash_hex, "old_bk_set_poseidon_hash") {
        Ok(b) => b,
        Err(msg) => return finalize_bk_update_failure(seq_no, &msg, last_seen_bk_update_seqno),
    };
    let l3 = match decode_hash32(&req.new_bk_set_poseidon_hash_hex, "new_bk_set_poseidon_hash") {
        Ok(b) => b,
        Err(msg) => return finalize_bk_update_failure(seq_no, &msg, last_seen_bk_update_seqno),
    };
    let h0 = match decode_hash32(&req.merkle_sibling_h0_hex, "merkle_sibling_h0") {
        Ok(b) => b,
        Err(msg) => return finalize_bk_update_failure(seq_no, &msg, last_seen_bk_update_seqno),
    };
    let h23 = match decode_hash32(&req.merkle_sibling_h23_hex, "merkle_sibling_h23") {
        Ok(b) => b,
        Err(msg) => return finalize_bk_update_failure(seq_no, &msg, last_seen_bk_update_seqno),
    };
    let block_id_fr = match ipc::fr_from_hex(&req.block_id_hex) {
        Ok(fr) => fr,
        Err(e) => return finalize_bk_update_failure(
            seq_no,
            &format!("invalid block_id_hex: {e}"),
            last_seen_bk_update_seqno,
        ),
    };
    // The chain's raw 32-byte block hash for the SHA-256 Merkle reconstruction.
    // `block_id_hex` is the Fr-reduced form (254-bit), so it cannot be used
    // here when the chain hash exceeds the Fr modulus (top 2 bits set on the
    // LE-interpreted big-endian byte string). The bundle carries the raw hash
    // separately in `block_id_hash_hex`.
    let block_id_bytes = match decode_hash32(&req.block_id_hash_hex, "block_id_hash") {
        Ok(b) => b,
        Err(msg) => return finalize_bk_update_failure(seq_no, &msg, last_seen_bk_update_seqno),
    };

    // (1) Attestation. L2 in the bundle MUST match the verifier's stored
    // commitment — this is what authorises the update.
    let attestation_verified = if l2 != state.stored_bk_set_commitment {
        warn!(
            "bk-update {}: L2 {} != stored commitment {} — attestation rejected without circuit run",
            seq_no,
            hex::encode(l2),
            hex::encode(state.stored_bk_set_commitment),
        );
        false
    } else {
        let bk_set_hash_fr = match ipc::fr_from_hex(&req.old_bk_set_poseidon_hash_hex) {
            Ok(fr) => fr,
            Err(e) => {
                return finalize_bk_update_failure(
                    seq_no,
                    &format!("L2 hex not decodable as Fr: {e}"),
                    last_seen_bk_update_seqno,
                )
            }
        };
        let primary_proof_bytes = match hex::decode(&req.primary_proof_hex) {
            Ok(b) => b,
            Err(e) => {
                return finalize_bk_update_failure(
                    seq_no,
                    &format!("invalid primary_proof_hex: {e}"),
                    last_seen_bk_update_seqno,
                )
            }
        };
        let public_instances = vec![
            block_id_fr,
            bk_set_hash_fr,
            Fr::from(req.block_seq_no as u64),
            Fr::from(req.last_seen_bk_update_seqno as u64),
        ];
        match req.attestation_circuit {
            ipc::AttestationCircuit::Primary => verifier::verify_primary_proof(
                key_manager,
                &primary_proof_bytes,
                &public_instances,
            ),
            ipc::AttestationCircuit::Fallback => verifier::verify_fallback_proof(
                key_manager,
                &primary_proof_bytes,
                &public_instances,
            ),
        }
    };

    // (2) Open SHA-256 Merkle: H1 = SHA(L2‖L3); H01 = SHA(H0‖H1); root = SHA(H01‖H23).
    // Compared against the raw 32-byte chain block hash (`block_id_hash_hex`),
    // not against `block_id_fr.to_repr()` — the chain hash is 256 bits and may
    // exceed the BN254 Fr modulus, so the Fr public instance is a lossy
    // 254-bit projection. Consistency between the two is enforced separately
    // in check (2b) below.
    let h1_calc = sha256_concat(&l2, &l3);
    let h01_calc = sha256_concat(&h0, &h1_calc);
    let root_calc = sha256_concat(&h01_calc, &h23);
    let merkle_verified = root_calc == block_id_bytes;
    if !merkle_verified {
        warn!(
            "bk-update {}: Merkle root mismatch — computed {} vs block_id_hash {}",
            seq_no,
            hex::encode(root_calc),
            hex::encode(block_id_bytes),
        );
    }

    // (2b) block_id_hash ↔ block_id_fr consistency. Without this, a bundle
    // could pair one block's raw hash (used by check 2) with another block's
    // Fr instance (bound by the Circuit 1a/1b proof in check 1). Both must
    // refer to the same chain block.
    //
    // The relation the prover establishes is `block_id_fr = Σ byte_i · 256^i
    // mod Fr_modulus` where byte_i are the chain hash bytes in LE order
    // (`compute_block_id_fr` in bridge-prover-lib::prover). We re-run that
    // construction here and require equality.
    let block_id_fr_from_hash = {
        let mut acc = Fr::zero();
        let mut power = Fr::one();
        let base = Fr::from(256u64);
        for &b in &block_id_bytes {
            acc += Fr::from(b as u64) * power;
            power *= base;
        }
        acc
    };
    let fr_consistency_ok = block_id_fr_from_hash == block_id_fr;
    if !fr_consistency_ok {
        warn!(
            "bk-update {}: block_id_fr inconsistent with block_id_hash — \
             Fr(hash mod p) != bundled Fr instance",
            seq_no
        );
    }

    // (3) Monotonicity. Matches `BridgeState::apply_bk_set_update` precondition.
    let monotonicity_ok = (req.block_seq_no as u64) > state.stored_last_bk_set_update_seq_no;
    if !monotonicity_ok {
        warn!(
            "bk-update {}: not monotone (last applied = {})",
            seq_no, state.stored_last_bk_set_update_seq_no
        );
    }

    let verify_ok =
        attestation_verified && merkle_verified && fr_consistency_ok && monotonicity_ok;
    if verify_ok {
        if let Err(e) = state.apply_bk_set_update(l2, l3, req.block_seq_no as u64) {
            // apply_bk_set_update re-checks the preconditions; if it
            // disagrees with what we just verified, something is racing
            // with us. Refuse to advance and surface the error.
            error!("bk-update {}: apply_bk_set_update failed: {}", seq_no, e);
            let r = ipc::BkUpdateResult {
                block_seq_no: seq_no,
                attestation_verified,
                merkle_verified,
                monotonicity_ok,
                verify_ok: false,
                error: Some(format!("apply failed: {e}")),
            };
            let _ = ipc::write_bk_update_result(&r);
            *last_seen_bk_update_seqno = seq_no;
            return;
        }
        if let Err(e) = state.save(state_file) {
            error!("bk-update {}: state save failed: {}", seq_no, e);
        } else {
            info!(
                "bk-update {}: APPLIED — stored_bk_set_commitment now {}, stored_last_bk_set_update_seq_no={}",
                seq_no,
                hex::encode(state.stored_bk_set_commitment),
                state.stored_last_bk_set_update_seq_no,
            );
        }
    } else {
        warn!(
            "bk-update {}: REJECTED (attestation={}, merkle={}, fr_consistency={}, monotone={})",
            seq_no, attestation_verified, merkle_verified, fr_consistency_ok, monotonicity_ok
        );
    }

    let r = ipc::BkUpdateResult {
        block_seq_no: seq_no,
        attestation_verified,
        merkle_verified,
        monotonicity_ok,
        verify_ok,
        error: if verify_ok {
            None
        } else {
            Some(format!(
                "attestation={}, merkle={}, fr_consistency={}, monotone={}",
                attestation_verified, merkle_verified, fr_consistency_ok, monotonicity_ok
            ))
        },
    };
    if let Err(e) = ipc::write_bk_update_result(&r) {
        error!("bk-update {}: failed to write result file: {}", seq_no, e);
    }
    *last_seen_bk_update_seqno = seq_no;
}

/// Decode a 32-byte hex string into a fixed-size array, with a descriptive
/// error message on failure.
fn decode_hash32(hex_str: &str, field: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_str).map_err(|e| format!("invalid {field}: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!(
            "invalid {field}: expected 32 bytes, got {}",
            bytes.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Helper for the early-return arms of `process_bk_update_bundle`. Records
/// the failure result file and advances the cursor so the next poll picks
/// up the following bundle instead of looping on this one.
fn finalize_bk_update_failure(seq_no: u32, msg: &str, last_seen: &mut u32) {
    error!("bk-update {}: {}", seq_no, msg);
    write_bk_update_failure(seq_no, msg);
    *last_seen = seq_no;
}

// =====================================================================
// Circuit 4 (event proof) verification
// =====================================================================
//
// The verifier daemon models the future Ethereum bridge contract. Circuit 4
// publishes exactly 10 public instances:
//   [token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
//    sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root]
// The 9 leading slots are bound to the proof by the circuit. The last
// slot, `final_root`, is the single Poseidon root the prover committed to;

//
// Acceptance gate: the daemon does an off-circuit membership check —
// `final_root` must appear somewhere in the daemon's currently mirrored
// `state.flatten_layer_hashes()` (= `MAX_LAYERS × W` = 10 × 128 = 1280
// entries, where `W = HISTORY_PROOF_WINDOW_SIZE`). This mirrors the
// contract sketch in `acki-nacki-to-eth-bridge-halo2-circuits/README.md`
// lines 810-853 (`submitWithdrawalProof`).
//
// TBD: the daemon does not yet enforce a `proven[]` map against the
// `nullifier` slot (replay protection), and `recipient_{hi,lo}` are not
// yet bound to the on-chain `msg.sender`.

/// On-disk schema for `proof_event_NNNNNN.json` produced by
/// `bridge-event-prove`. Kept private (`Deserialize`-only) here so the
/// daemon does not couple to the prover binary's full output schema.
#[derive(Deserialize)]
struct EventProofFile {
    /// Bumped by the prover whenever the file shape changes; the daemon
    /// hard-fails on a mismatch so the orchestrator notices the drift.
    schema_version: u32,
    seq_no: u32,
    proof_hex: String,
    public_instances_hex: Vec<String>,
    /// Prover-side self-verify outcome — informational; the daemon's own
    /// verify is the actual acceptance gate.
    self_verified: bool,
}

const EVENT_PROOF_INPUT_SCHEMA_VERSION: u32 = 1;
const EVENT_PROOF_RESULT_SCHEMA_VERSION: u32 = 1;

/// Result written next to the input as `proof_event_NNNNNN.result.json`.
/// Schema is independent of the input — bump independently if the result
/// shape evolves.
#[derive(Serialize)]
struct EventProofResult<'a> {
    schema_version: u32,
    seq_no: u32,
    /// Final accept/reject. `verified == anchor_matched && proof_valid`.
    verified: bool,
    /// Whether the proof's `final_root` (public instance slot 9) was found
    /// in the daemon's current `flatten_layer_hashes()` snapshot.
    anchor_matched: bool,
    /// Whether the halo2 verifier accepted the proof against the event
    /// VK and the supplied public instances. Not run if anchor mismatched.
    proof_valid: bool,
    /// Prover-side self-verify outcome — passed through for forensics.
    prover_self_verified: bool,
    /// Daemon's `stored_last_seen_block_height` at the moment of verify —
    /// the equivalent of the contract's `storedLastSeenBlockHeight` snapshot.
    verified_at_block_height: u64,
    /// Daemon's `stored_last_seen_block_seq_no` at the moment of verify.
    verified_at_block_seq_no: u64,
    /// Echo of the public-instance hex passed in, so a downstream consumer
    /// (the Python orchestrator) can assert exact pass-through.
    event_public_instances_hex: &'a [String],
    error: Option<String>,
}

fn event_result_file_path(seq_no: u32) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("proofs/proof_event_{:06}.result.json", seq_no))
}

/// Scan `proofs/` for any `proof_event_NNNNNN.json` with seq_no > last_seen.
/// Returns the lowest unseen seq_no, or `None` if nothing new.
fn find_next_event_proof_file(last_seen: i64) -> Option<u32> {
    let dir = match std::fs::read_dir("proofs") {
        Ok(d) => d,
        Err(_) => return None,
    };
    let mut candidates: Vec<u32> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Match `proof_event_NNNNNN.json` exactly — NOT
            // `proof_event_NNNNNN.result.json` (the daemon's own output).
            if name.starts_with("proof_event_") && name.ends_with(".json")
                && !name.ends_with(".result.json")
            {
                let num_str = name
                    .trim_start_matches("proof_event_")
                    .trim_end_matches(".json");
                num_str.parse::<u32>().ok()
            } else {
                None
            }
        })
        .filter(|&seq| (seq as i64) > last_seen)
        .collect();
    candidates.sort();
    candidates.first().copied()
}

/// Verify a single event proof file and write its `.result.json` sibling.
/// All failures are non-fatal — they're recorded in `stats` and in the
/// on-disk result file so the orchestrator can diagnose.
fn process_event_proof(
    seq_no: u32,
    key_manager: &KeyManager,
    state: &BridgeState,
    stats: &mut Stats,
) {
    stats.event_total += 1;
    let t_start = Instant::now();

    // ---- Load and parse the input file ----
    let path = format!("proofs/proof_event_{:06}.json", seq_no);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("read error: {}", e);
            error!("event {}: {}", seq_no, msg);
            write_event_failure(seq_no, &[], false, state, &msg);
            stats.event_proof_invalid += 1;
            stats.event_failures.push((seq_no, msg));
            return;
        }
    };
    let file: EventProofFile = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("parse error: {}", e);
            error!("event {}: {}", seq_no, msg);
            write_event_failure(seq_no, &[], false, state, &msg);
            stats.event_proof_invalid += 1;
            stats.event_failures.push((seq_no, msg));
            return;
        }
    };
    if file.schema_version != EVENT_PROOF_INPUT_SCHEMA_VERSION {
        let msg = format!(
            "schema mismatch: file v{} != daemon v{}",
            file.schema_version, EVENT_PROOF_INPUT_SCHEMA_VERSION
        );
        error!("event {}: {}", seq_no, msg);
        write_event_failure(seq_no, &file.public_instances_hex, file.self_verified, state, &msg);
        stats.event_proof_invalid += 1;
        stats.event_failures.push((seq_no, msg));
        return;
    }
    if file.seq_no != seq_no {
        warn!(
            "event {}: file's internal seq_no={} disagrees with filename — using filename",
            seq_no, file.seq_no
        );
    }

    // ---- Decode proof bytes + public instances ----
    let proof_bytes = match hex::decode(&file.proof_hex) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("invalid proof_hex: {}", e);
            error!("event {}: {}", seq_no, msg);
            write_event_failure(seq_no, &file.public_instances_hex, file.self_verified, state, &msg);
            stats.event_proof_invalid += 1;
            stats.event_failures.push((seq_no, msg));
            return;
        }
    };

    // Public instance layout (per `event_verifier.rs`):
    //   [token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
    //    sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root]
    // The circuit now publishes a single `final_root` slot; the verifier
    // checks it off-circuit against `state.flatten_layer_hashes()` below.
    let expected_num_instances = 10;
    if file.public_instances_hex.len() != expected_num_instances {
        let msg = format!(
            "expected {} public instances, got {}",
            expected_num_instances,
            file.public_instances_hex.len()
        );
        error!("event {}: {}", seq_no, msg);
        write_event_failure(seq_no, &file.public_instances_hex, file.self_verified, state, &msg);
        stats.event_proof_invalid += 1;
        stats.event_failures.push((seq_no, msg));
        return;
    }
    let mut instances: Vec<Fr> = Vec::with_capacity(expected_num_instances);
    for (i, h) in file.public_instances_hex.iter().enumerate() {
        match ipc::fr_from_hex(h) {
            Ok(fr) => instances.push(fr),
            Err(e) => {
                let msg = format!("instance[{}] decode error: {}", i, e);
                error!("event {}: {}", seq_no, msg);
                write_event_failure(seq_no, &file.public_instances_hex, file.self_verified, state, &msg);
                stats.event_proof_invalid += 1;
                stats.event_failures.push((seq_no, msg));
                return;
            }
        }
    }

    // ---- Anchor check (the "current bridge state" gate) ----
    //
    // The circuit publishes a single `final_root` (instance slot 9). The
    // verifier accepts the proof iff that root matches one of the layer
    // hashes the daemon currently mirrors in `state.layer_windows` — the
    // off-circuit replacement for the old in-circuit candidate vector.
    let current_hashes = state.flatten_layer_hashes();
    debug_assert_eq!(current_hashes.len(), MAX_LAYERS * state.window_size);
    let final_root_bytes: [u8; 32] = instances[9].to_repr();
    let anchor_matched = current_hashes.iter().any(|h| *h == final_root_bytes);
    if !anchor_matched {
        let msg = format!(
            "anchor mismatch — final_root {} not found in current layer_windows",
            hex::encode(final_root_bytes),
        );
        warn!("event {}: {}", seq_no, msg);
        let result = EventProofResult {
            schema_version: EVENT_PROOF_RESULT_SCHEMA_VERSION,
            seq_no,
            verified: false,
            anchor_matched: false,
            proof_valid: false,
            prover_self_verified: file.self_verified,
            verified_at_block_height: state.stored_last_seen_block_height,
            verified_at_block_seq_no: state.stored_last_seen_block_seq_no,
            event_public_instances_hex: &file.public_instances_hex,
            error: Some(msg.clone()),
        };
        if let Err(e) = write_event_result(&result) {
            error!("event {}: failed to write result file: {}", seq_no, e);
        }
        stats.event_anchor_mismatch += 1;
        stats.event_failures.push((seq_no, msg));
        return;
    }

    // ---- Cryptographic verification ----
    let t_verify = Instant::now();
    let proof_valid = event_verifier::verify_event_proof(key_manager, &proof_bytes, &instances);
    let verify_elapsed = t_verify.elapsed();
    info!(
        "event {}: Circuit 4 {} ({:?}) | total {:?}",
        seq_no,
        if proof_valid { "VERIFIED" } else { "FAILED" },
        verify_elapsed,
        t_start.elapsed(),
    );

    let result = EventProofResult {
        schema_version: EVENT_PROOF_RESULT_SCHEMA_VERSION,
        seq_no,
        verified: proof_valid,
        anchor_matched: true,
        proof_valid,
        prover_self_verified: file.self_verified,
        verified_at_block_height: state.stored_last_seen_block_height,
        verified_at_block_seq_no: state.stored_last_seen_block_seq_no,
        event_public_instances_hex: &file.public_instances_hex,
        error: if proof_valid {
            None
        } else {
            Some("halo2 verify_proof rejected the Circuit 4 proof".to_string())
        },
    };
    if let Err(e) = write_event_result(&result) {
        error!("event {}: failed to write result file: {}", seq_no, e);
    }

    if proof_valid {
        stats.event_verified_ok += 1;
    } else {
        stats.event_proof_invalid += 1;
        stats.event_failures.push((seq_no, "proof rejected".to_string()));
    }
}

fn write_event_result(result: &EventProofResult<'_>) -> anyhow::Result<()> {
    let path = event_result_file_path(result.seq_no);
    let json = serde_json::to_string_pretty(result)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Helper used by every early-return path above to emit a uniform failure
/// result. Keeps the various error branches from diverging in shape.
fn write_event_failure(
    seq_no: u32,
    public_instances_hex: &[String],
    prover_self_verified: bool,
    state: &BridgeState,
    error: &str,
) {
    let result = EventProofResult {
        schema_version: EVENT_PROOF_RESULT_SCHEMA_VERSION,
        seq_no,
        verified: false,
        anchor_matched: false,
        proof_valid: false,
        prover_self_verified,
        verified_at_block_height: state.stored_last_seen_block_height,
        verified_at_block_seq_no: state.stored_last_seen_block_seq_no,
        event_public_instances_hex: public_instances_hex,
        error: Some(error.to_string()),
    };
    if let Err(e) = write_event_result(&result) {
        error!("event {}: failed to write failure result: {}", seq_no, e);
    }
}
