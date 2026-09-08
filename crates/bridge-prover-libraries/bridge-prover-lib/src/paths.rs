//! Runtime resolution of daemon persistence paths.
//!
//! All bridge daemons (`bridge-prover-daemon`, `bridge-verifier-daemon`,
//! `bridge-relayer-daemon`) persist runtime state + proof-IPC files under a
//! pair of directories:
//!
//!   * **state dir**  — `prover_state.json`, `verifier_state.json`,
//!     `prover_bk_set.json`, `bootstrap_seed.json`
//!   * **proofs dir** — combined-bundle proof/result JSON, bk-update
//!     proof/result JSON, event-proof JSON
//!
//! Historically these were hardcoded to `./state` and `./proofs`, with
//! ad-hoc `_l2` suffixes when L2 anchoring landed. That produced parallel
//! layouts (`state/` vs `state_l2/`, `proofs/` vs `proofs_l2/`) and made
//! Case-1-vs-Case-7 in the runbook read as if L2 was a structurally
//! different case when the only real difference was where files lived.
//!
//! Resolution order (per var):
//!   1. Explicit override `BRIDGE_STATE_DIR` / `BRIDGE_PROOFS_DIR` — win.
//!   2. Else if `BRIDGE_CONFIG_DIR` is set → derive as
//!      `<config>/state` / `<config>/proofs`. This is how the
//!      `L1_config/` / `L2_config/` layout hangs together: sourcing
//!      `L2_config/env` sets `BRIDGE_CONFIG_DIR=./L2_config`, and both
//!      the state dir and the proofs dir fall out of that.
//!   3. Else fall back to legacy `state` / `proofs` — preserves the
//!      exact old behavior for anyone still running without env vars,
//!      and keeps the ipc.rs unit tests (which assert literal
//!      `"proofs/..."` paths) passing without env setup.

use std::path::PathBuf;

pub const ENV_CONFIG_DIR: &str = "BRIDGE_CONFIG_DIR";
pub const ENV_STATE_DIR: &str = "BRIDGE_STATE_DIR";
pub const ENV_PROOFS_DIR: &str = "BRIDGE_PROOFS_DIR";

/// Legacy default for the state directory. Matches the pre-refactor
/// hardcoded `./state`, minus the `./` (kept string-identical to
/// pre-existing `create_dir_all("state")` calls).
pub const DEFAULT_STATE_DIR: &str = "state";

/// Legacy default for the proofs IPC directory. Matches the pre-refactor
/// `ipc::PROOFS_DIR` constant exactly, so tests asserting
/// `"proofs/bkupd_000042.json"` continue to hold with no env set.
pub const DEFAULT_PROOFS_DIR: &str = "proofs";

/// The `BRIDGE_CONFIG_DIR` env value if set, else `None`. Not cached —
/// tests and short-lived binaries can mutate the environment between
/// calls without stale reads.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os(ENV_CONFIG_DIR).map(PathBuf::from)
}

/// Resolved state directory. See module docs for precedence.
pub fn state_dir() -> PathBuf {
    if let Some(v) = std::env::var_os(ENV_STATE_DIR) {
        return PathBuf::from(v);
    }
    if let Some(cfg) = config_dir() {
        return cfg.join("state");
    }
    PathBuf::from(DEFAULT_STATE_DIR)
}

/// Resolved proofs-IPC directory. See module docs for precedence.
pub fn proofs_dir() -> PathBuf {
    if let Some(v) = std::env::var_os(ENV_PROOFS_DIR) {
        return PathBuf::from(v);
    }
    if let Some(cfg) = config_dir() {
        return cfg.join("proofs");
    }
    PathBuf::from(DEFAULT_PROOFS_DIR)
}

// Convenience helpers for the four state-file names both daemons share.
// Keeping the file-name literals in one place so a future rename touches
// exactly one line.

pub fn prover_state_file() -> PathBuf {
    state_dir().join("prover_state.json")
}

pub fn prover_bk_set_file() -> PathBuf {
    state_dir().join("prover_bk_set.json")
}

pub fn verifier_state_file() -> PathBuf {
    state_dir().join("verifier_state.json")
}

pub fn bootstrap_seed_file() -> PathBuf {
    state_dir().join("bootstrap_seed.json")
}

/// Ensure the resolved state directory exists (idempotent).
pub fn ensure_state_dir() {
    let _ = std::fs::create_dir_all(state_dir());
}

/// Ensure the resolved proofs directory exists (idempotent).
pub fn ensure_proofs_dir() {
    let _ = std::fs::create_dir_all(proofs_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises every test that touches the three env vars.
    ///
    /// Snapshot-and-restore is enough for one test at a time and for
    /// nothing else, which is what the guard below used to claim: it
    /// reasoned about `--test-threads=1`, and that is not how the harness
    /// runs. These tests share ONE process and the environment is
    /// process-global, so `defaults_match_legacy` clearing
    /// `BRIDGE_CONFIG_DIR` to check the defaults and
    /// `config_dir_derives_both` setting it are the same variable, and
    /// which lands first is a race. Measured before this lock: two runs
    /// in eight failed, on a different one of the four tests each time.
    ///
    /// A flaky suite is worse here than a failing one. `bridge-prover-lib`
    /// already carries two environmental fixture failures, so its result
    /// line is read by eye against a remembered baseline — and a count
    /// that moves on its own is how a real regression gets waved through
    /// as "the usual two".
    ///
    /// Held for the whole test rather than just the setup: the reads
    /// under assertion are as much of the critical section as the writes.
    ///
    /// Poison is recovered, not propagated. These tests assert, so one of
    /// them failing is an ordinary outcome; letting that poison the mutex
    /// would turn a single real failure into three more that say nothing
    /// about themselves.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A minimal env guard for tests that mutate `BRIDGE_STATE_DIR` /
    /// `BRIDGE_PROOFS_DIR` / `BRIDGE_CONFIG_DIR`. Snapshots the three vars
    /// and restores them on drop, holding [`ENV_LOCK`] for as long as it
    /// lives so no sibling test observes the window in between.
    struct EnvGuard {
        saved: [(&'static str, Option<std::ffi::OsString>); 3],
        /// Dropped after `Drop for EnvGuard` has restored the vars —
        /// `Drop::drop` runs before any field is dropped, so the lock is
        /// still held while the restore happens.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let vars = [ENV_CONFIG_DIR, ENV_STATE_DIR, ENV_PROOFS_DIR];
            let mut saved: [(&'static str, Option<std::ffi::OsString>); 3] = [
                (ENV_CONFIG_DIR, None),
                (ENV_STATE_DIR, None),
                (ENV_PROOFS_DIR, None),
            ];
            for (i, v) in vars.iter().enumerate() {
                saved[i].0 = v;
                saved[i].1 = std::env::var_os(v);
                std::env::remove_var(v);
            }
            Self {
                saved,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn defaults_match_legacy() {
        let _g = EnvGuard::new();
        assert_eq!(state_dir(), PathBuf::from("state"));
        assert_eq!(proofs_dir(), PathBuf::from("proofs"));
    }

    #[test]
    fn config_dir_derives_both() {
        let _g = EnvGuard::new();
        std::env::set_var(ENV_CONFIG_DIR, "./L2_config");
        assert_eq!(state_dir(), PathBuf::from("./L2_config/state"));
        assert_eq!(proofs_dir(), PathBuf::from("./L2_config/proofs"));
    }

    #[test]
    fn explicit_overrides_win() {
        let _g = EnvGuard::new();
        std::env::set_var(ENV_CONFIG_DIR, "./L1_config");
        std::env::set_var(ENV_STATE_DIR, "/tmp/override_state");
        std::env::set_var(ENV_PROOFS_DIR, "/tmp/override_proofs");
        assert_eq!(state_dir(), PathBuf::from("/tmp/override_state"));
        assert_eq!(proofs_dir(), PathBuf::from("/tmp/override_proofs"));
    }

    #[test]
    fn state_file_helpers_compose() {
        let _g = EnvGuard::new();
        std::env::set_var(ENV_STATE_DIR, "L2_config/state");
        assert_eq!(
            prover_state_file(),
            PathBuf::from("L2_config/state/prover_state.json")
        );
        assert_eq!(
            verifier_state_file(),
            PathBuf::from("L2_config/state/verifier_state.json")
        );
        assert_eq!(
            prover_bk_set_file(),
            PathBuf::from("L2_config/state/prover_bk_set.json")
        );
        assert_eq!(
            bootstrap_seed_file(),
            PathBuf::from("L2_config/state/bootstrap_seed.json")
        );
    }
}
