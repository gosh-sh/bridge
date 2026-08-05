//! Shared BK-set bootstrap helpers for the prover and relayer daemons.
//!
//! Both daemons face the same problem on cold start: they need a
//! `HashMap<u16, Vec<u8>>` of active BLS pubkeys before `LiveProverDriver`
//! can be built, and both must protect against the "stale ./state on top of
//! a re-initialised chain" footgun by comparing the on-disk
//! `prover_bk_set.commitment` against the current
//! `BRIDGE_BK_SET_CONFIG` file.
//!
//! Historically this logic lived only in `bridge-prover-daemon/src/main.rs`
//! and `bridge-relayer-daemon/src/bin/relayer.rs::run_daemon_live` diverged
//! (file-only mode, no startup guard). Extracting the two functions here
//! keeps them in lockstep: any fix to one daemon's bootstrap semantics
//! automatically applies to the other.
//!
//! The two daemons resolve the config-file path differently (prover-daemon
//! reads [`ENV_BK_SET_CONFIG`] with a hardcoded default; relayer-daemon
//! resolves it from a clap flag), so the helpers take the path as an
//! explicit argument. See [`resolve_bk_set_config_path`] for the shared
//! env-based resolver the prover-daemon uses.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context};
use tracing::info;

use bridge_gql_fetcher::gql_client::GqlClient;
use bridge_poseidon as poseidon;

use crate::prover_bk_set::ProverBkSet;

/// Default JSON path for the genesis BK-set config file. Both daemons
/// override in normal operation; the default is kept for local scripts
/// that ship a `bk_set.local.json` alongside the binary.
pub const DEFAULT_BK_SET_CONFIG: &str = "./bk_set.local.json";

/// Env var naming the JSON file holding the genesis (or current) BK set.
/// Consulted by [`resolve_bk_set_config_path`] and by
/// prover-daemon-style callers.
pub const ENV_BK_SET_CONFIG: &str = "BRIDGE_BK_SET_CONFIG";

/// Env var selecting the bootstrap mode. Values:
///
///   - `file` (default): treat the JSON file as the current BK set. Correct
///     on fresh chains and on rotating chains booted from genesis.
///   - `fold_at_height`: treat the JSON file as the *genesis* snapshot and
///     apply all `bkSetUpdates` up to [`ENV_BK_SET_TARGET_SEQNO`] (or the
///     caller-supplied `explicit_bootstrap_seqno` if that env is unset)
///     via `bk_set_at_height`. Use this when cold-starting the daemon
///     against a long-running chain whose committee has rotated many
///     times since genesis.
///
/// Consulted only on first bootstrap; a persisted `prover_bk_set.json` is
/// the source of truth on Resume regardless of this setting.
pub const ENV_BK_SET_BOOTSTRAP: &str = "BRIDGE_BK_SET_BOOTSTRAP";

/// Env var supplying the target height for `fold_at_height` mode. Optional
/// — if unset, [`load_bk_set`] falls back to the caller's
/// `explicit_bootstrap_seqno` argument.
pub const ENV_BK_SET_TARGET_SEQNO: &str = "BRIDGE_BK_SET_TARGET_SEQNO";

/// Read [`ENV_BK_SET_CONFIG`] with fallback to [`DEFAULT_BK_SET_CONFIG`].
/// Convenience for callers that resolve the path from env (prover-daemon
/// style). Relayer-daemon builds the path from its clap flag and does not
/// use this.
pub fn resolve_bk_set_config_path() -> String {
    std::env::var(ENV_BK_SET_CONFIG).unwrap_or_else(|_| DEFAULT_BK_SET_CONFIG.to_string())
}

/// Load the initial BK set. Mode is selected via [`ENV_BK_SET_BOOTSTRAP`]:
///
///   - `file` (default): read JSON at `bk_set_config` verbatim — correct
///     for fresh chains and for cold starts anchored at genesis.
///   - `fold_at_height`: read JSON as the *genesis* snapshot, then apply
///     all `bkSetUpdates` up to [`ENV_BK_SET_TARGET_SEQNO`] (falls back to
///     `explicit_bootstrap_seqno` if unset) via
///     `bk_set_fetcher::bk_set_at_height`.
///
/// A persisted `prover_bk_set.json` (loaded downstream) overrides this on
/// Resume, so this function is only load-bearing on first-ever startup.
pub async fn load_bk_set(
    gql: &GqlClient,
    bk_set_config: &str,
    explicit_bootstrap_seqno: Option<u64>,
) -> anyhow::Result<HashMap<u16, Vec<u8>>> {
    let json = bridge_gql_fetcher::bk_set_fetcher::load_bk_set_from_config(bk_set_config)
        .with_context(|| format!("failed to load BK set from config file {}", bk_set_config))?;

    let mode = std::env::var(ENV_BK_SET_BOOTSTRAP).unwrap_or_else(|_| "file".to_string());
    match mode.as_str() {
        "file" => {
            info!(
                "BK-set bootstrap: mode=file, loaded {} signers from {}",
                json.len(),
                bk_set_config
            );
            Ok(json)
        }
        "fold_at_height" => {
            let target = match std::env::var(ENV_BK_SET_TARGET_SEQNO) {
                Ok(s) => s.parse::<u64>().with_context(|| {
                    format!("{} must be a u64, got '{}'", ENV_BK_SET_TARGET_SEQNO, s)
                })?,
                Err(_) => explicit_bootstrap_seqno.ok_or_else(|| {
                    anyhow::format_err!(
                        "BRIDGE_BK_SET_BOOTSTRAP=fold_at_height requires either \
                         {} to be set or an explicit bootstrap_seqno to be passed in",
                        ENV_BK_SET_TARGET_SEQNO,
                    )
                })?,
            };
            info!(
                "BK-set bootstrap: mode=fold_at_height, genesis={} signers, target_height={}",
                json.len(),
                target
            );
            bridge_gql_fetcher::bk_set_fetcher::bk_set_at_height(gql, json, target)
                .await
                .with_context(|| format!("bk_set_at_height failed for target_height={}", target))
        }
        other => bail!(
            "unknown {}='{}', expected 'file' or 'fold_at_height'",
            ENV_BK_SET_BOOTSTRAP,
            other
        ),
    }
}

/// File-first startup guard: if `bk_set_config` points to a readable file,
/// compare its Poseidon commitment against `prover_bk_set.commitment`.
/// Runs before any GQL call — cheap and catches the common devnet
/// stale-state footgun immediately.
///
/// Behaviour:
/// * Match — silent pass.
/// * File missing — silent pass (config was hand-set to a path we don't
///   own; the runtime L2 check in `bk_update.rs` remains as a safety net).
/// * Mismatch AND `last_applied_update_seq_no == 0` — BAIL. The daemon
///   has never processed a rotation, so a fresh chain overwriting the
///   seed file while `./state/` persisted from the previous chain
///   instance is the overwhelmingly likely explanation.
/// * Mismatch AND `last_applied_update_seq_no > 0` — INFO log only.
///   Expected on any chain where BK rotation is enabled and the seed
///   file is only the genesis snapshot; the daemon has legitimately
///   moved past it via the bk-update lane.
pub fn verify_prover_bk_set_matches_config_file(
    bk_set_config: &str,
    prover_bk_set: &ProverBkSet,
) -> anyhow::Result<()> {
    if !Path::new(bk_set_config).exists() {
        info!(
            "chain-config check skipped: {} not found (this is fine on \
             warm restarts where the file is intentionally absent)",
            bk_set_config,
        );
        return Ok(());
    }
    let file_pubkeys = bridge_gql_fetcher::bk_set_fetcher::load_bk_set_from_config(bk_set_config)
        .with_context(|| format!("load {} for startup guard", bk_set_config))?;
    let (_, file_commitment) = poseidon::compute_bk_set_poseidon(&file_pubkeys);
    if file_commitment == prover_bk_set.commitment {
        info!(
            "chain-config check OK: {} matches prover_bk_set.commitment",
            bk_set_config,
        );
        return Ok(());
    }
    if prover_bk_set.last_applied_update_seq_no == 0 {
        anyhow::bail!(
            "chain-config check FAILED:\n  \
             {} commitment: {}\n  \
             prover_bk_set:      {}\n\
             prover_bk_set.last_applied_update_seq_no == 0 — the daemon \
             has never processed a rotation, so this almost certainly \
             means the chain was re-initialised (fresh devnet zerostate \
             rewrote {}) while ./state/ persisted from the previous \
             chain instance. Wipe ./state/ and restart, or restore the \
             paired {} from backup.",
            bk_set_config,
            hex::encode(file_commitment),
            hex::encode(prover_bk_set.commitment),
            bk_set_config,
            bk_set_config,
        );
    }
    info!(
        "chain-config check: {} commitment {} != prover_bk_set {} (cursor {}) \
         — daemon has rotated past the genesis snapshot; not an issue",
        bk_set_config,
        hex::encode(file_commitment),
        hex::encode(prover_bk_set.commitment),
        prover_bk_set.last_applied_update_seq_no,
    );
    Ok(())
}
