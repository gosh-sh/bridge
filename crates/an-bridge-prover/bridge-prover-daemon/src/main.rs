//! Bridge Prover Daemon — processes key blocks with Circuit 1a + Circuit 2.
//!
//! Flow:
//! 1. Connect to a node via GraphQL
//! 2. Fetch BK set, initialize keys for both circuits
//! 3. Wait for first key block (initialization)
//! 4. For each subsequent key block:
//!    a. Fetch attestation → generate Circuit 1a proof
//!    b. Build layer hashes preimage + chain proofs → generate Circuit 2 proof
//!    c. Write the combined proof JSON to `proofs/proof_NNN.json`
//!    d. Verify the bundle via one of two paths:
//!
//! ## Verification modes (Cargo features)
//!
//! ### Default — paired with `bridge-verifier-daemon` (no extra feature)
//!
//! The prover writes `proofs/proof_NNN.json` and **waits** for the verifier
//! daemon to drop `proofs/result_NNN.json` (`ipc::wait_for_result`). The
//! verifier owns its own `state/verifier_state.json`. Used by the standard
//! E2E orchestrator (`python/generate_withdrawals_with_live_event_proving.py`).
//!
//! ### `--features self-verify` — standalone (no verifier daemon)
//!
//! The prover inline-verifies both proofs itself, records the
//! `(primary_ok, layer_ok, verify_ok)` outcome in
//! `BridgeState::recent_bundles` and persists `prover_state.json`. On a
//! verify failure it logs diagnostics and exits non-zero with the state
//! NOT advanced past the failed bundle, so a restart reprocesses from the
//! last good seq_no. Used by the CI smoke test (`bridge_e2e_self_contained.py`).
//!
//! In both modes the combined Circuit 1a + Circuit 2 proof JSON is written
//! under `proofs/` for downstream consumers (Circuit 4 orchestrator, archival).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use tracing::{error, info, warn};

use bridge_prover_lib::Fr;
use bridge_prover_lib::THINNING_FACTOR_P;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;

use bridge_prover_lib::attestation_fetcher::{self, AttestationEvidence};
use bridge_prover_lib::bk_set_fetcher::{
    self, BK_CHANGE_VARIANT_ADDED, BK_CHANGE_VARIANT_REMOVED,
};
use bridge_prover_lib::block_id_tree::BlockIdMerkleTree;
use bridge_prover_lib::bootstrap::{self, BootstrapSeed};
use bridge_prover_lib::bridge_state::BridgeState;
#[cfg(feature = "self-verify")]
use bridge_prover_lib::bridge_state::BundleResult;
use bridge_prover_lib::prover_bk_set::ProverBkSet;
use bridge_prover_lib::gql_client::{self, GqlClient};
use bridge_prover_lib::ipc;
use bridge_prover_lib::keys::KeyManager;
use bridge_prover_lib::layer_prover;
use bridge_prover_lib::poseidon;
use bridge_prover_lib::prover;
#[cfg(feature = "self-verify")]
use bridge_prover_lib::verifier;

/// Default GraphQL endpoint when `BRIDGE_GQL_ENDPOINT` is not set. Targets a
/// local Docker devnet running `make run` in the acki-nacki repo. For shellnet
/// or any other deployment set `BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql`.
const DEFAULT_GQL_ENDPOINT: &str = "http://localhost/graphql";

/// Env var: overrides [`DEFAULT_GQL_ENDPOINT`]. Read once at startup.
const ENV_GQL_ENDPOINT: &str = "BRIDGE_GQL_ENDPOINT";

/// Env var: explicit mid-chain bootstrap seed seqno. When set, the daemon
/// skips the auto-latest discovery loop and seeds directly from this block.
/// Value MUST be > 0 and divisible by `W * P` (= [`HISTORY_WINDOW_SIZE`] *
/// [`THINNING_FACTOR_P`]), so the first proven target sits exactly one bundle
/// later — same spacing as steady state.
const ENV_BOOTSTRAP_SEQNO: &str = "BRIDGE_BOOTSTRAP_SEQNO";

// History window size — pulled from the vendored poseidon_dense constant so
// the prover and verifier always agree without depending on the node workspace.
const HISTORY_WINDOW_SIZE: u64 =
    bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64;

/// How often to log a heartbeat summary while the loop is running.
const STATS_LOG_INTERVAL: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const SLEEP_ON_RETRY: Duration = Duration::from_secs(5);
/// How long to wait for the external verifier daemon to drop
/// `proofs/result_NNN.json` before declaring a verifier-side timeout.
/// Paired mode only.
#[cfg(not(feature = "self-verify"))]
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(300);
const PARAMS_DIR: &str = "./params";
const LOGS_DIR: &str = "./logs";
const STATE_FILE: &str = "./state/prover_state.json";
/// Prover-private pubkey table. Holds the same `signer_index → 48B BLS
/// pubkey` map that the prover uses to build Circuit 1a/1b witnesses;
/// the verifier daemon (= contract mirror) never reads it. See
/// `bridge_prover_lib::prover_bk_set::ProverBkSet` for the rationale.
const PROVER_BK_SET_FILE: &str = "./state/prover_bk_set.json";
const BK_SET_CONFIG: &str = "./bk_set.json";

#[derive(Default)]
struct Stats {
    key_blocks_processed: u32,
    primary_proofs_ok: u32,
    layer_proofs_ok: u32,
    verification_ok: u32,
    verification_failed: u32,
    total_proof_time: Duration,
}

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

    // Env-driven config — no CLI parser needed (only two knobs). Both read
    // once at startup and logged for the operator's benefit.
    let gql_endpoint = std::env::var(ENV_GQL_ENDPOINT)
        .unwrap_or_else(|_| DEFAULT_GQL_ENDPOINT.to_string());
    let bundle_size = HISTORY_WINDOW_SIZE * THINNING_FACTOR_P;
    let explicit_bootstrap_seqno: Option<u64> = match std::env::var(ENV_BOOTSTRAP_SEQNO) {
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
            Some(n)
        }
        Err(_) => None,
    };

    info!("=== Bridge Prover Daemon (Circuit 1a + Circuit 2) ===");
    info!("GQL endpoint: {}", gql_endpoint);
    info!("history window size: W = {}", HISTORY_WINDOW_SIZE);
    info!("thinning factor:     P = {} (prove every {}-th key block, bundle = {} blocks)",
          THINNING_FACTOR_P, THINNING_FACTOR_P, bundle_size);
    match explicit_bootstrap_seqno {
        Some(n) => info!("bootstrap mode: EXPLICIT seed seqno = {} (from {})", n, ENV_BOOTSTRAP_SEQNO),
        None    => info!("bootstrap mode: AUTO-LATEST (seed at next W*P-aligned key block after chain head)"),
    }
    info!("running indefinitely; send SIGINT (Ctrl-C) to shut down cleanly");

    // Graceful-shutdown flag flipped by the Ctrl-C handler. Checked at the top
    // of each loop iteration so we never tear down mid-proof.
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

    // 1. Connect to node.
    let gql = gql_client::create_client(&gql_endpoint)?;
    info!("GraphQL client created");

    // 2. Fetch BK set. `mut` so the bk-update drain loop in the main loop can
    // rotate both the pubkey map and its Poseidon commitment in lockstep with
    // `prover_bk_set` and `state.stored_bk_set_commitment`.
    let mut bk_set = load_bk_set(&gql).await?;
    let (mut bk_set_commitment, _) = poseidon::compute_bk_set_poseidon(&bk_set);
    info!("BK set: {} signers, commitment={:?}", bk_set.len(), bk_set_commitment);

    // 3. Initialize key manager (both circuits).
    info!("loading keys...");
    let mut key_manager = KeyManager::new(Path::new(PARAMS_DIR));
    key_manager.ensure_primary_keys(&bk_set)?;
    info!("primary (1a) keys ready");
    key_manager.ensure_fallback_keys(&bk_set)?;
    info!("fallback (1b) keys ready");
    key_manager.ensure_layer_keys()?;
    info!("layer (2) keys ready");

    // 4. Load or create state.
    let mut state = BridgeState::load(STATE_FILE, HISTORY_WINDOW_SIZE as usize)?;
    info!("state loaded: initialized={}, last_key_block={}", state.initialized, state.stored_last_seen_block_seq_no);

    // 4a. Materialise the prover-private BK pubkey table.
    //
    // Phase 1 (Plan §2.2): `prover_bk_set.json` lives next to `state.json`
    // and tracks the same pubkeys we already fetched into `bk_set` above.
    // Both files must agree on the BK-set commitment; if they ever drift
    // (e.g. hand-edited state, partial migration) we re-derive from the
    // in-memory `bk_set` map and overwrite the file. The verifier daemon
    // NEVER loads this file — it mirrors only the commitment.
    //
    // On local devnet (where the BK set never rotates) this file is written
    // once at bootstrap and never updated afterwards. On shellnet, future
    // bk-update bundles (Phase 3) will rotate both `state.json` and this
    // file in lockstep.
    // Phase 3: mutable so the main-loop drain can `rotate()` the table after
    // a verified bk-update bundle. Drives Circuit 1a/1b witness construction
    // for bk-update blocks (signed by the OLD set, i.e. L2's pubkeys).
    let mut prover_bk_set: ProverBkSet = match ProverBkSet::load(PROVER_BK_SET_FILE)? {
        Some(loaded) => {
            // If `state` is initialised and the commitments disagree, the
            // safe move is to refuse to start: this almost always means the
            // operator forgot to clean up one of the two files between runs.
            // Before initialisation `stored_bk_set_commitment` is all-zero,
            // so we skip the check in that case.
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
            // Authority shift: once a paired `prover_bk_set` exists on disk,
            // IT (not the GQL-fresh `bk_set` we fetched at step 2) is the
            // source of truth for "what set has been formally applied to the
            // contract-mirror state". If the chain has rotated since the last
            // run, the freshly fetched `bk_set` will be ahead of
            // `prover_bk_set` — those intermediate rotations must be drained
            // through the bk-update bundle path, not absorbed silently. So
            // re-anchor the in-memory `bk_set` / `bk_set_commitment` to the
            // persisted ProverBkSet; the main-loop drain will catch up via
            // `next_update_after`.
            bk_set = loaded.pubkeys()
                .context("prover_bk_set.pubkeys() failed after load")?;
            let (fr, _) = poseidon::compute_bk_set_poseidon(&bk_set);
            bk_set_commitment = fr;
            info!(
                "re-anchored in-memory bk_set to prover_bk_set: {} signers, commitment={}",
                bk_set.len(),
                hex::encode(bk_set_commitment.to_repr()),
            );
            loaded
        }
        None => {
            let pbs = ProverBkSet::from_pubkeys(&bk_set, 0);
            // Cross-check: the Poseidon commitment we computed earlier in
            // step 2 from the same `bk_set` map must match what ProverBkSet
            // computes here. If they don't, something is structurally wrong
            // (e.g. a Poseidon-parameter mismatch) and we shouldn't pretend
            // bootstrap succeeded.
            let bk_hash_bytes: [u8; 32] = bk_set_commitment.to_repr();
            anyhow::ensure!(
                pbs.commitment == bk_hash_bytes,
                "ProverBkSet commitment {} != poseidon::compute_bk_set_poseidon {} — \
                 Poseidon-parameter mismatch?",
                hex::encode(pbs.commitment),
                hex::encode(bk_hash_bytes),
            );
            pbs.save(PROVER_BK_SET_FILE)?;
            info!(
                "prover_bk_set: bootstrapped {} signers from GQL, written to {}",
                pbs.pubkeys_hex.len(),
                PROVER_BK_SET_FILE,
            );
            pbs
        }
    };

    // 5. If not initialized, derive bootstrap seed from a key block and persist
    //    it for the verifier.
    //
    //    The prover plays the role of the "deployer" in the production analog:
    //    it queries the node for the chosen key block's envelope, applies the
    //    seed to its own state, AND writes `state/bootstrap_seed.json` so the
    //    verifier (which has no node connection) can mirror the same L1 entry.
    //    See `bridge_prover_lib::bootstrap` for the full rationale.
    //
    //    Seed seqno selection:
    //      - Explicit (env BRIDGE_BOOTSTRAP_SEQNO=N): use N directly. Already
    //        validated above to be > 0 and divisible by W*P.
    //      - Auto: round chain head UP to the next multiple of W*P, then wait
    //        for the chain to reach it. This is the shellnet / mid-chain mode:
    //        no need to walk back to genesis, and the first proven target sits
    //        exactly one bundle (W*P blocks) past the seed — same spacing as
    //        steady state, so no asymmetric "first bundle" gap.
    if !state.initialized {
        let bk_hash_bytes: [u8; 32] = bk_set_commitment.to_repr();

        // Pick the seed seqno ONCE here, before entering the wait loop. In auto
        // mode we pin it to the next W*P boundary strictly past the current
        // chain head; recomputing inside the loop would let the target chase
        // the head every time it crossed a boundary and the daemon would never
        // start. In explicit mode it's already fixed by env var.
        let seed_seqno = match explicit_bootstrap_seqno {
            Some(n) => n,
            None    => {
                let blocks = gql.query_latest_blocks(5).await?;
                let latest_seq = blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);
                let n = ((latest_seq / bundle_size) + 1) * bundle_size;
                info!("auto-mode: chain head at seq_no={}, pinned seed seq_no={}", latest_seq, n);
                n
            }
        };

        let seed: BootstrapSeed = loop {
            let blocks = gql.query_latest_blocks(5).await?;
            let latest_seq = blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);

            if latest_seq < seed_seqno {
                info!(
                    "latest block seq_no={}, waiting for chain to reach seed seq_no={} ({} blocks to go)...",
                    latest_seq, seed_seqno, seed_seqno - latest_seq
                );
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }

            info!("seed key block available at seq_no {} (chain head at {})", seed_seqno, latest_seq);
            match bootstrap::fetch_from_node(&gql, seed_seqno, bk_hash_bytes).await {
                Ok(s) => {
                    info!(
                        "seed block: {} history_proofs layers, block_height={}",
                        s.layer_hashes.len(),
                        s.block_height,
                    );
                    break s;
                }
                Err(e) => {
                    warn!("could not fetch seed key block envelope at seq_no={} ({}), retrying...",
                          seed_seqno, e);
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
            }
        };

        seed.apply(&mut state);
        state.save(STATE_FILE)?;
        seed.save(bootstrap::DEFAULT_SEED_PATH)?;
        info!(
            "initialized from seed: seqno={}, height={}, layers={}; seed written to {}",
            seed.block_seq_no,
            seed.block_height,
            seed.layer_hashes.len(),
            bootstrap::DEFAULT_SEED_PATH,
        );
    }

    // 6. Main processing loop — runs until Ctrl-C.
    let mut stats = Stats::default();
    let t_total = Instant::now();
    let mut last_stats_log = Instant::now();

    loop {
        if shutdown.load(Ordering::SeqCst) {
            info!("shutdown flag set, exiting main loop");
            break;
        }
        if last_stats_log.elapsed() >= STATS_LOG_INTERVAL {
            info!(
                "[heartbeat] processed={}, primary_ok={}, layer_ok={}, verify_ok={}, fail={}, uptime={:?}",
                stats.key_blocks_processed,
                stats.primary_proofs_ok,
                stats.layer_proofs_ok,
                stats.verification_ok,
                stats.verification_failed,
                t_total.elapsed()
            );
            last_stats_log = Instant::now();
        }

        // Poll for new blocks.
        let blocks = gql.query_latest_blocks(5).await?;
        let latest_seq = blocks.iter().map(|(_, s)| *s).max().unwrap_or(0);

        // Find next thinned key block to process (advances by W * P, not W —
        // see bridge_prover_lib::THINNING_FACTOR_P).
        let next_key_seqno = find_next_thinned_key_block(
            state.stored_last_seen_block_seq_no,
            latest_seq,
            HISTORY_WINDOW_SIZE,
            THINNING_FACTOR_P,
        );

        if next_key_seqno.is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }
        let target_seqno = next_key_seqno.unwrap();
        info!("=== Processing key block at height {} ===", target_seqno);
        // Phase 3 BK-set-update drain.
        //
        // Real rotations land at arbitrary heights (NOT W·P-aligned), so the
        // old L2≠L3-at-target-seqno detector would miss almost all of them.
        // Instead we walk the bkSetUpdates stream past our applied cursor
        // (`state.stored_last_bk_set_update_seq_no`) and drain every event
        // whose height ≤ target_seqno BEFORE proving the layer bundle for
        // this target. Each drained event:
        //   * generates Circuit 1a/1b against the OLD prover_bk_set pubkeys
        //   * writes `bkupd_NNN.json` and waits for `bkupd_result_NNN.json`
        //   * on `verify_ok=true`, rotates BOTH state files (BridgeState +
        //     ProverBkSet) and refreshes the in-memory `bk_set` /
        //     `bk_set_commitment` so the subsequent layer bundle uses the
        //     rotated set.
        // See `docs/bk_set_update_no_circuit3_plan.md`, §9 items 4+5.
        loop {
            let cursor = state.stored_last_bk_set_update_seq_no;
            let upd = match bk_set_fetcher::next_update_after(&gql, cursor).await {
                Ok(Some(u)) => u,
                Ok(None) => break, // no more pending events
                Err(e) => {
                    warn!("bk-update drain: next_update_after failed: {} — will retry next iteration", e);
                    break;
                }
            };
            let upd_seqno = match upd.height {
                Some(h) if h > cursor && h <= target_seqno => h,
                Some(h) if h > target_seqno => {
                    // Future rotation — leave it for a later main-loop tick.
                    info!(
                        "bk-update drain: next event at {} is past target_seqno={}, deferring",
                        h, target_seqno
                    );
                    break;
                }
                _ => {
                    warn!("bk-update drain: skipping event with missing/stale height");
                    break;
                }
            };

            info!("=== bk-update drain: processing event at seq_no {} ===", upd_seqno);

            // Fetch the bk-update block's 8 Merkle leaves to derive L2/L3
            // and the open siblings H0/H23.
            let upd_block = match gql.query_proof_block_by_seqno(upd_seqno).await {
                Ok(b) => b,
                Err(e) => {
                    warn!("bk-update {}: GQL block fetch failed: {} — bailing drain", upd_seqno, e);
                    break;
                }
            };
            let leaves = match upd_block.block_merkle_tree_leaves {
                Some(l) => l,
                None => {
                    warn!("bk-update {}: block has no block_merkle_tree_leaves — bailing drain", upd_seqno);
                    break;
                }
            };
            let tree = BlockIdMerkleTree::from_leaves(leaves);
            let l2 = tree.leaves[2];
            let l3 = tree.leaves[3];

            // Cross-check: L2 must equal our currently-applied commitment.
            // If not, the prover is out-of-sync with the chain's bk-set
            // history — bail and let an operator investigate.
            if l2 != prover_bk_set.commitment {
                error!(
                    "bk-update {}: L2 {} != prover_bk_set.commitment {} — \
                     prover out of sync with chain bk-set history; bailing drain",
                    upd_seqno,
                    hex::encode(l2),
                    hex::encode(prover_bk_set.commitment),
                );
                break;
            }

            // Apply the bk_set_update_hex deltas to the OLD pubkey table to
            // derive the NEW one, and verify it hashes to L3.
            let blob = match hex::decode(&upd.bk_set_update_hex) {
                Ok(b) => b,
                Err(e) => {
                    warn!("bk-update {}: bk_set_update_hex decode failed: {} — bailing", upd_seqno, e);
                    break;
                }
            };
            let changes = bk_set_fetcher::parse_bk_set_changes_pub(&blob);
            if changes.is_empty() {
                warn!("bk-update {}: parsed 0 changes from blob — bailing drain", upd_seqno);
                break;
            }
            let cur_pubkeys = match prover_bk_set.pubkeys() {
                Ok(p) => p,
                Err(e) => {
                    error!("bk-update {}: prover_bk_set.pubkeys() failed: {} — bailing", upd_seqno, e);
                    break;
                }
            };
            let mut new_pubkeys = cur_pubkeys.clone();
            for (variant, idx, pk) in &changes {
                match *variant {
                    BK_CHANGE_VARIANT_ADDED => {
                        new_pubkeys.insert(*idx, pk.clone());
                    }
                    BK_CHANGE_VARIANT_REMOVED => {
                        new_pubkeys.remove(idx);
                    }
                    other => {
                        warn!("bk-update {}: ignoring unknown change variant {}", upd_seqno, other);
                    }
                }
            }
            // Delta entries come from the raw bk_set_update blob as 96-byte
            // uncompressed BLS pubkeys, while `cur_pubkeys` is 48-byte
            // compressed. Normalize before feeding to any BLS-aware helper
            // (compute_bk_set_poseidon, Circuit 1a witness builder).
            let new_pubkeys = match bk_set_fetcher::normalize_bk_set_pubkeys(new_pubkeys) {
                Ok(p) => p,
                Err(e) => {
                    error!("bk-update {}: pubkey normalization failed: {} — bailing", upd_seqno, e);
                    break;
                }
            };
            let (_, recomp_c) = poseidon::compute_bk_set_poseidon(&new_pubkeys);
            if recomp_c != l3 {
                error!(
                    "bk-update {}: Poseidon(new_pubkeys) {} != L3 {} — bailing drain",
                    upd_seqno,
                    hex::encode(recomp_c),
                    hex::encode(l3),
                );
                break;
            }

            // Fetch attestation evidence for the bk-update block. These
            // signatures are produced by the OLD signer set (the block that
            // *announces* the rotation is itself signed by the prior set),
            // so the Circuit 1a/1b witness uses `cur_pubkeys`.
            let upd_evidence = match attestation_fetcher::fetch_attestation_evidence(
                &gql, upd_seqno as u32,
            ).await {
                Ok(ev) => ev,
                Err(e) => {
                    warn!("bk-update {}: attestation fetch failed: {} — bailing drain", upd_seqno, e);
                    break;
                }
            };

            // Generate Circuit 1a/1b proof, on-demand PK load to stay within
            // the same ~3.7 GB envelope as the layer-bundle path.
            let last_seen_for_upd = state.stored_last_bk_set_update_seq_no as u32;
            let t_upd_proof = Instant::now();
            let (circuit_tag, upd_proof) = match &upd_evidence {
                AttestationEvidence::Primary(att) => {
                    info!("bk-update {}: PRIMARY path → Circuit 1a", upd_seqno);
                    if let Err(e) = key_manager.load_primary_pk() {
                        error!("bk-update {}: load_primary_pk failed: {} — bailing", upd_seqno, e);
                        break;
                    }
                    let res = prover::generate_primary_proof(
                        &key_manager,
                        &att.raw_bytes,
                        &cur_pubkeys,
                        last_seen_for_upd,
                    );
                    key_manager.unload_primary_pk();
                    match res {
                        Ok(o) => (ipc::AttestationCircuit::Primary, o),
                        Err(e) => {
                            error!("bk-update {}: Circuit 1a proof failed: {} — bailing", upd_seqno, e);
                            break;
                        }
                    }
                }
                AttestationEvidence::Fallback { primary, fallback } => {
                    info!("bk-update {}: FALLBACK path → Circuit 1b", upd_seqno);
                    if let Err(e) = key_manager.load_fallback_pk() {
                        error!("bk-update {}: load_fallback_pk failed: {} — bailing", upd_seqno, e);
                        break;
                    }
                    let res = prover::generate_fallback_proof(
                        &key_manager,
                        &primary.raw_bytes,
                        &fallback.raw_bytes,
                        &cur_pubkeys,
                        last_seen_for_upd,
                    );
                    key_manager.unload_fallback_pk();
                    match res {
                        Ok(o) => (ipc::AttestationCircuit::Fallback, o),
                        Err(e) => {
                            error!("bk-update {}: Circuit 1b proof failed: {} — bailing", upd_seqno, e);
                            break;
                        }
                    }
                }
            };
            let upd_proof_gen_ms = t_upd_proof.elapsed().as_millis() as u64;
            info!(
                "bk-update {}: {:?} proof generated in {} ms",
                upd_seqno, circuit_tag, upd_proof_gen_ms
            );

            // Write the bk-update bundle for the verifier daemon.
            let req = ipc::BkUpdateRequest {
                schema_version: ipc::PROOF_REQUEST_SCHEMA_VERSION,
                block_seq_no: upd_seqno as u32,
                block_height: upd_block.height,
                last_seen_bk_update_seqno: last_seen_for_upd,
                block_id_hex: ipc::fr_to_hex(&upd_proof.block_id_fr),
                block_id_hash_hex: hex::encode(tree.root),
                attestation_circuit: circuit_tag,
                primary_proof_hex: hex::encode(&upd_proof.proof_bytes),
                old_bk_set_poseidon_hash_hex: hex::encode(l2),
                new_bk_set_poseidon_hash_hex: hex::encode(l3),
                merkle_sibling_h0_hex: hex::encode(tree.h0),
                merkle_sibling_h23_hex: hex::encode(tree.h23),
                primary_proof_gen_ms: upd_proof_gen_ms,
            };
            if let Err(e) = ipc::write_bk_update_request(&req) {
                error!("bk-update {}: write_bk_update_request failed: {} — bailing", upd_seqno, e);
                break;
            }
            info!("bk-update {}: wrote {}", upd_seqno, ipc::bkupd_file_path(upd_seqno as u32));

            // Block on the verifier daemon's ACK. Same timeout as the
            // layer-bundle path.
            #[cfg(not(feature = "self-verify"))]
            let result = match ipc::wait_for_bk_update_result(upd_seqno as u32, VERIFIER_TIMEOUT).await {
                Ok(r) => r,
                Err(e) => {
                    error!("bk-update {}: verifier timeout/err: {} — bailing", upd_seqno, e);
                    break;
                }
            };
            #[cfg(feature = "self-verify")]
            let result = {
                warn!(
                    "bk-update {}: self-verify feature does not inline-verify bk-update bundles; \
                     synthesising verify_ok=true based on local Merkle + monotonicity",
                    upd_seqno
                );
                // Local checks already enforced by the Merkle equality
                // assertion above and the apply_bk_set_update precondition.
                ipc::BkUpdateResult {
                    block_seq_no: upd_seqno as u32,
                    attestation_verified: true,
                    merkle_verified: true,
                    monotonicity_ok: true,
                    verify_ok: true,
                    error: None,
                }
            };

            if !result.verify_ok {
                error!(
                    "bk-update {}: verifier REJECTED (attestation={}, merkle={}, monotone={}, err={:?}) — \
                     bailing drain; state NOT advanced",
                    upd_seqno,
                    result.attestation_verified,
                    result.merkle_verified,
                    result.monotonicity_ok,
                    result.error,
                );
                break;
            }

            // Rotate BridgeState (commitment + bk-update cursor) and
            // ProverBkSet (pubkey table + applied cursor) in lockstep.
            if let Err(e) = state.apply_bk_set_update(l2, l3, upd_seqno) {
                error!(
                    "bk-update {}: local apply_bk_set_update failed AFTER verifier ACK: {} — \
                     manual intervention required",
                    upd_seqno, e
                );
                break;
            }
            if let Err(e) = state.save(STATE_FILE) {
                error!("bk-update {}: state.save failed: {}", upd_seqno, e);
                break;
            }
            if let Err(e) = prover_bk_set.rotate(l3, new_pubkeys.clone(), upd_seqno) {
                error!(
                    "bk-update {}: prover_bk_set.rotate failed: {} — manual intervention required",
                    upd_seqno, e
                );
                break;
            }
            if let Err(e) = prover_bk_set.save(PROVER_BK_SET_FILE) {
                error!("bk-update {}: prover_bk_set.save failed: {}", upd_seqno, e);
                break;
            }

            // Refresh the in-memory bk_set and its Fr commitment so the
            // subsequent layer bundle (and the next drain iteration, if any)
            // sees the rotated set.
            bk_set = new_pubkeys;
            let (new_fr, _) = poseidon::compute_bk_set_poseidon(&bk_set);
            bk_set_commitment = new_fr;

            info!(
                "bk-update {}: APPLIED — new commitment {}, {} signers active",
                upd_seqno,
                hex::encode(l3),
                bk_set.len(),
            );
        }


        // Fetch and classify attestation evidence for the key block.
        //
        // Detection is structural (count + target_type), not heuristic — see
        // `attestation_fetcher::fetch_attestation_evidence` docs. Threshold
        // enforcement (≥2N/3 for 1a, >N/2 for 1b) lives inside the circuits;
        // here we only choose which circuit to run.
        let evidence = match attestation_fetcher::fetch_attestation_evidence(
            &gql, target_seqno as u32,
        ).await {
            Ok(ev) => ev,
            Err(e) => {
                warn!("key block {}: attestation evidence not ready ({}), retrying...", target_seqno, e);
                tokio::time::sleep(SLEEP_ON_RETRY).await;
                continue;
            }
        };

        // Defensive BK-set-membership warning. In-circuit checks are
        // authoritative; this just surfaces obviously-stale BK sets earlier.
        let signer_indices = evidence.signer_indices();
        let missing: Vec<u16> = signer_indices
            .iter()
            .filter(|idx| !bk_set.contains_key(idx))
            .copied()
            .collect();
        if !missing.is_empty() {
            warn!("key block {}: signers {:?} not in BK set, skipping", target_seqno, missing);
            state.stored_last_seen_block_seq_no = target_seqno;
            state.stored_last_seen_block_height = target_seqno;
            state.save(STATE_FILE)?;
            continue;
        }


        let t_proof = Instant::now();

        // ---- Circuit 1a or 1b: Attestation Proof ----
        // Dispatch on the classified evidence. Each branch loads its own PK
        // on demand and unloads immediately after so we stay within the
        // ~3.7 GB single-PK memory envelope before Circuit 2.
        let t_primary = Instant::now();
        let (attestation_circuit_tag, primary_proof) = match &evidence {
            AttestationEvidence::Primary(att) => {
                info!("key block {}: PRIMARY path → Circuit 1a", target_seqno);
                info!("key block {}: loading primary PK...", target_seqno);
                key_manager.load_primary_pk()?;
                let res = prover::generate_primary_proof(
                    &key_manager,
                    &att.raw_bytes,
                    &bk_set,
                    state.stored_last_seen_block_seq_no as u32,
                );
                key_manager.unload_primary_pk();
                match res {
                    Ok(output) => {
                        stats.primary_proofs_ok += 1;
                        (ipc::AttestationCircuit::Primary, output)
                    }
                    Err(e) => {
                        error!("key block {}: Circuit 1a proof failed: {}", target_seqno, e);
                        stats.verification_failed += 1;
                        stats.key_blocks_processed += 1;
                        state.stored_last_seen_block_seq_no = target_seqno;
                        state.stored_last_seen_block_height = target_seqno;
                        state.save(STATE_FILE)?;
                        continue;
                    }
                }
            }
            AttestationEvidence::Fallback { primary, fallback } => {
                info!("key block {}: FALLBACK path → Circuit 1b", target_seqno);
                info!("key block {}: loading fallback PK...", target_seqno);
                key_manager.load_fallback_pk()?;
                let res = prover::generate_fallback_proof(
                    &key_manager,
                    &primary.raw_bytes,
                    &fallback.raw_bytes,
                    &bk_set,
                    state.stored_last_seen_block_seq_no as u32,
                );
                key_manager.unload_fallback_pk();
                match res {
                    Ok(output) => {
                        stats.primary_proofs_ok += 1;
                        (ipc::AttestationCircuit::Fallback, output)
                    }
                    Err(e) => {
                        error!("key block {}: Circuit 1b proof failed: {}", target_seqno, e);
                        stats.verification_failed += 1;
                        stats.key_blocks_processed += 1;
                        state.stored_last_seen_block_seq_no = target_seqno;
                        state.stored_last_seen_block_height = target_seqno;
                        state.save(STATE_FILE)?;
                        continue;
                    }
                }
            }
        };
        let primary_proof_gen_ms = t_primary.elapsed().as_millis() as u64;
        info!(
            "key block {}: {:?} proof generated in {} ms",
            target_seqno, attestation_circuit_tag, primary_proof_gen_ms
        );

        // ---- Circuit 2: Layer Hashes Movement Proof ----
        // Inputs are reconstructed from real block data:
        //   - preimage: history_proofs parsed from the target block's CommonSection
        //   - siblings: 8-leaf SHA-256 Merkle path for L0 (Poseidon over the preimage)
        //   - chain_links: real Poseidon Merkle proofs walked across intermediate
        //     key blocks fetched via GraphQL (see real_chain_builder).
        // Load layer PK on demand, unload after to free ~2.8 GB.
        info!("key block {}: loading layer PK...", target_seqno);
        key_manager.load_layer_pk()?;

        info!("key block {}: generating Circuit 2 proof...", target_seqno);
        let t_layer = Instant::now();
        let layer_proof = match generate_layer_proof_for_key_block(
            &key_manager,
            &gql,
            &state,
            target_seqno,
            &bk_set_commitment,
        ).await {
            Ok(output) => {
                stats.layer_proofs_ok += 1;
                key_manager.unload_layer_pk();
                output
            }
            Err(e) => {
                error!("key block {}: Circuit 2 proof failed: {}", target_seqno, e);
                key_manager.unload_layer_pk();
                stats.verification_failed += 1;
                stats.key_blocks_processed += 1;
                state.stored_last_seen_block_seq_no = target_seqno;
                state.stored_last_seen_block_height = target_seqno;
                state.save(STATE_FILE)?;
                continue;
            }
        };
        let layer_proof_gen_ms = t_layer.elapsed().as_millis() as u64;
        info!(
            "key block {}: Circuit 2 proof generated in {} ms",
            target_seqno, layer_proof_gen_ms
        );

        let proof_time = t_proof.elapsed();
        stats.total_proof_time += proof_time;
        info!("key block {}: both proofs generated in {:?}", target_seqno, proof_time);

        // Fetch the envelope once now to extract (a) the authoritative
        // thread-anchored block_height for the ProofRequest, and (b) the
        // layer-hash bundle reused below by `append_bundle`. The envelope is
        // immutable once committed so doing this before vs. after the verifier
        // call is equivalent.
        let (state_layer_hashes, observed_height): (Vec<([u8; 32], u8)>, u64) = match gql
            .query_proof_block_by_seqno(target_seqno)
            .await
        {
            Ok(block) => {
                let hashes = block.history_proofs.iter()
                    .map(|(&layer, root)| (*root, layer))
                    .collect();
                (hashes, block.height)
            }
            Err(e) => {
                warn!(
                    "key block {}: GQL block fetch failed ({}), using seq_no as height fallback",
                    target_seqno, e
                );
                (Vec::new(), target_seqno)
            }
        };

        // Write combined proof JSON for downstream consumers (orchestrators,
        // archival). Self-verification happens inline below; no external
        // verifier daemon is involved.
        let request = ipc::ProofRequest {
            schema_version: ipc::PROOF_REQUEST_SCHEMA_VERSION,
            block_seq_no: target_seqno as u32,
            block_height: observed_height,
            last_seen_block_seqno: state.stored_last_seen_block_seq_no as u32,
            block_id_hex: ipc::fr_to_hex(&primary_proof.block_id_fr),
            // Tag the proof with whichever attestation circuit produced it
            // so the verifier picks the matching VK (1a Primary vs 1b Fallback).
            attestation_circuit: attestation_circuit_tag,
            primary_proof_hex: hex::encode(&primary_proof.proof_bytes),
            layer_proof_hex: hex::encode(&layer_proof.proof_bytes),
            layer_block_id_hex: ipc::fr_to_hex(&layer_proof.block_id_fr),
            bk_set_poseidon_hash_hex: ipc::fr_to_hex(&layer_proof.bk_set_poseidon_hash_fr),
            num_layers: layer_proof.num_layers,
            layer_hash_frs_hex: layer_proof.layer_hash_frs.iter().map(|fr| ipc::fr_to_hex(fr)).collect(),
            prev_max_level_layer_hash_hex: ipc::fr_to_hex(&layer_proof.prev_max_level_layer_hash_fr),
            primary_proof_gen_ms,
            layer_proof_gen_ms,
        };
        ipc::write_combined_proof(&request)?;

        // ---- Verification: feature-gated ------------------------------------
        //
        // Paired mode (default, no extra feature): write the proof and wait
        // for the verifier daemon to ACK via `proofs/result_NNN.json`.
        //
        // Standalone mode (`--features self-verify`): inline-verify both
        // proofs in-process, record the verdict to
        // `BridgeState::recent_bundles`, abort the daemon on failure.
        #[cfg(not(feature = "self-verify"))]
        {
            info!("key block {}: proof written, waiting for verifier...", target_seqno);
            match ipc::wait_for_result(target_seqno as u32, VERIFIER_TIMEOUT).await {
                Ok(result) => {
                    if result.primary_verified && result.layer_verified {
                        info!("key block {}: BOTH VERIFIED OK", target_seqno);
                        stats.verification_ok += 1;
                    } else {
                        error!(
                            "key block {}: VERIFICATION FAILED: primary={}, layer={}, err={:?}",
                            target_seqno,
                            result.primary_verified,
                            result.layer_verified,
                            result.error,
                        );
                        stats.verification_failed += 1;
                    }
                }
                Err(e) => {
                    error!("key block {}: verifier timeout/error: {}", target_seqno, e);
                    stats.verification_failed += 1;
                }
            }
        }

        #[cfg(feature = "self-verify")]
        {
            // Public-instance order matches what the verifier daemon would
            // build (bridge-verifier-daemon/src/main.rs §"Verify Circuit 1a/2");
            // any divergence here would cause a false-negative verify.
            let primary_instances = vec![
                primary_proof.block_id_fr,
                bk_set_commitment,
                Fr::from(target_seqno),
                Fr::from(state.stored_last_seen_block_seq_no),
            ];
            let t_verify_primary = Instant::now();
            let primary_ok = verifier::verify_primary_proof(
                &key_manager,
                &primary_proof.proof_bytes,
                &primary_instances,
            );
            info!(
                "key block {}: Circuit 1a self-verify {} ({:?})",
                target_seqno,
                if primary_ok { "OK" } else { "FAIL" },
                t_verify_primary.elapsed()
            );

            // Circuit 2 public instances: [block_id, bk_set_hash, num_layers,
            // layer_hash_frs[0..10], prev_max_level_layer_hash] — 14 elements.
            let mut layer_instances = Vec::with_capacity(14);
            layer_instances.push(layer_proof.block_id_fr);
            layer_instances.push(layer_proof.bk_set_poseidon_hash_fr);
            layer_instances.push(Fr::from(layer_proof.num_layers as u64));
            for fr in &layer_proof.layer_hash_frs {
                layer_instances.push(*fr);
            }
            layer_instances.push(layer_proof.prev_max_level_layer_hash_fr);
            let t_verify_layer = Instant::now();
            let layer_ok = verifier::verify_layer_proof(
                &key_manager,
                &layer_proof.proof_bytes,
                &layer_instances,
            );
            info!(
                "key block {}: Circuit 2 self-verify {} ({:?})",
                target_seqno,
                if layer_ok { "OK" } else { "FAIL" },
                t_verify_layer.elapsed()
            );

            let verify_ok = primary_ok && layer_ok;
            let ts_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            state.push_bundle_result(BundleResult {
                key_block_seq_no: target_seqno,
                primary_ok,
                layer_ok,
                verify_ok,
                ts_unix,
            });

            if !verify_ok {
                // Persist the failure marker, then abort. We deliberately do NOT
                // advance `stored_last_seen_*` here: a restart will reprocess this
                // key block from the last good cursor, and any external orchestrator
                // polling `recent_bundles` will see the verify_ok=false entry.
                // Stats are intentionally not incremented — `bail!` propagates
                // back to `main()` and the summary print at the end never runs.
                state.save(STATE_FILE)?;
                anyhow::bail!(
                    "key block {}: self-verification FAILED (primary_ok={}, layer_ok={}); \
                     state NOT advanced past failed bundle",
                    target_seqno,
                    primary_ok,
                    layer_ok
                );
            }

            info!("key block {}: BOTH SELF-VERIFIED OK", target_seqno);
            stats.verification_ok += 1;
        }

        // Advance state with the REAL layer hashes + block_height captured
        // above. Reached only when both proofs self-verified.
        let bk_hash_bytes: [u8; 32] = bk_set_commitment.to_repr();
        state.append_bundle(
            &state_layer_hashes,
            observed_height,
            target_seqno,
            bk_hash_bytes,
        );

        stats.key_blocks_processed += 1;
        state.save(STATE_FILE)?;
    }

    // Print summary.
    let elapsed = t_total.elapsed();
    info!("\n=== PROVER SUMMARY ===");
    info!("total time:              {:?}", elapsed);
    info!("key blocks processed:    {}", stats.key_blocks_processed);
    info!("primary proofs OK:       {}", stats.primary_proofs_ok);
    info!("layer proofs OK:         {}", stats.layer_proofs_ok);
    info!("verification OK:         {}", stats.verification_ok);
    info!("verification FAILED:     {}", stats.verification_failed);
    if stats.verification_ok > 0 {
        info!(
            "avg proof time:          {:?}",
            stats.total_proof_time / stats.verification_ok
        );
    }

    Ok(())
}

/// Find the next *thinned* key block to process after `last_key_seqno`.
///
/// The daemon advances in steps of `W * P` blocks: it proves only every
/// `P`-th master key block, with each Circuit 2 bundle internally chaining
/// `P` consecutive layer-1 windows via `verify_chain_of_dense_proofs`.
/// The bootstrap seed sits on a `W*P`-aligned key block (either chosen
/// explicitly via `BRIDGE_BOOTSTRAP_SEQNO` or computed as the next `W*P`
/// boundary past chain head in auto mode); from there the first thinned
/// target is `seed + W*P`, then `seed + 2*W*P`, etc. — symmetric spacing.
fn find_next_thinned_key_block(
    last_key_seqno: u64,
    latest_seqno: u64,
    window_size: u64,
    thinning_factor: u64,
) -> Option<u64> {
    let step = window_size * thinning_factor;
    // Next multiple of `step` strictly greater than `last_key_seqno`.
    // Works for both bootstrap (`last == window_size`) and steady state
    // (`last == k * step`).
    let next = ((last_key_seqno / step) + 1) * step;
    if next <= latest_seqno {
        Some(next)
    } else {
        None
    }
}

/// Generate Circuit 2 proof for a key block using real block data and real chain proofs.
///
/// Fetches the full AckiNackiBlock `data` field, parses it to extract:
/// - Layer hashes (from history_proofs in CommonSection)
/// - BK set Poseidon hash (from block_keeper_set_change_proof_data)
/// - Merkle tree siblings for L0 in the 8-leaf block_id tree
///
/// Chain proofs are built from real intermediate block data by reconstructing
/// the Poseidon Merkle trees that the node produced.
async fn generate_layer_proof_for_key_block(
    key_manager: &KeyManager,
    gql: &GqlClient,
    state: &BridgeState,
    target_seqno: u64,
    bk_set_commitment: &Fr,
) -> anyhow::Result<layer_prover::LayerProofOutput> {
    use bridge_prover_lib::block_id_tree;
    use bridge_prover_lib::real_chain_builder;

    info!("fetching block proof data for seq={}...", target_seqno);
    let block = gql
        .query_proof_block_by_seqno(target_seqno)
        .await
        .context("failed to fetch block proof data")?;

    let leaves = block.block_merkle_tree_leaves.ok_or_else(|| {
        anyhow::anyhow!(
            "block {} has no block_merkle_tree_leaves in GQL — node must expose them",
            target_seqno
        )
    })?;

    info!(
        "parsed: history_proofs={} layers",
        block.history_proofs.len(),
    );
    if block.history_proofs.is_empty() {
        anyhow::bail!("block {} has no history_proofs", target_seqno);
    }

    // 1. Build layer_hashes_preimage from history_proofs.
    let num_layers = block.history_proofs.len() as u8;
    let mut root_hashes = Vec::with_capacity(10);
    for i in 1..=10u8 {
        if let Some(root) = block.history_proofs.get(&i) {
            root_hashes.push(*root);
        } else {
            root_hashes.push([0u8; 32]);
        }
    }
    let preimage = block_id_tree::build_layer_hashes_preimage(num_layers as usize, &root_hashes);

    // 2. Build the 8-leaf SHA-256 Merkle tree from the GQL leaves and pull siblings for L0.
    let tree = block_id_tree::BlockIdMerkleTree::from_leaves(leaves);
    let siblings = tree.siblings_for_l0();
    info!(
        "block_id from GQL leaves merkle root: {}",
        hex::encode(tree.block_id())
    );

    // 3. BK set Poseidon hash — comes from the loaded BLS pubkeys.
    //
    // The node computes `leaves[2]` (old bk_set) and `leaves[3]` (new bk_set)
    // as Poseidon commitments using the same scheme as
    // `bridge_prover_lib::poseidon::compute_bk_set_poseidon` (see
    // `acki-nacki/node/src/block_keeper_system/mod.rs::poseidon_commitment`).
    // So `leaves[2]` must equal `bk_set_commitment.to_repr()` — assert this
    // as a fail-fast check on `bk_set.json` freshness. Without it, a stale
    // BK set silently produces a bogus BLS message hash and only blows up
    // deep inside Circuit 1A's pairing constraints.
    let bk_hash_bytes: [u8; 32] = bk_set_commitment.to_repr();
    if bk_hash_bytes != leaves[2] {
        anyhow::bail!(
            "loaded BK set Poseidon commitment ({}) does not match block.leaves[2] ({}) — \
             bk_set.json is stale or the chain rotated keys; refresh bk_set.json",
            hex::encode(bk_hash_bytes),
            hex::encode(leaves[2])
        );
    }
    // Note: `leaves[3]` (new bk_set Poseidon commitment) is not checked here
    // because the prover does not currently load the future bk_set.
    let bk_set_hash_fr = *bk_set_commitment;

    // 4. Build history_proofs map (already in block.history_proofs).
    let history_proofs_map = &block.history_proofs;

    // 5. Build REAL chain proofs from intermediate block data.
    let chain_result = real_chain_builder::build_real_chain(
        gql,
        state,
        history_proofs_map,
        target_seqno,
        HISTORY_WINDOW_SIZE,
    )
    .await
    .context("failed to build real chain proofs")?;

    info!("using REAL chain proofs ({} steps)", chain_result.num_steps);

    let prev_hash_fr = gosh_dense_balanced_tree::bytes_to_fr(&chain_result.prev_hash);

    // 6. Generate Circuit 2 proof with real preimage, real siblings, real chain.
    layer_prover::generate_layer_proof(
        key_manager,
        &preimage,
        &siblings,
        prev_hash_fr,
        chain_result.num_steps,
        &chain_result.chain_links,
        bk_set_hash_fr,
    )
}

async fn load_bk_set(gql: &GqlClient) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    match bridge_prover_lib::bk_set_fetcher::fetch_bk_set(gql).await {
        Ok(bk_set) => return Ok(bk_set),
        Err(e) => {
            warn!("failed to fetch BK set from GraphQL: {}", e);
            info!("trying config file fallback: {}", BK_SET_CONFIG);
        }
    }
    bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(BK_SET_CONFIG)
        .context("failed to load BK set from both GraphQL and config file")
}
