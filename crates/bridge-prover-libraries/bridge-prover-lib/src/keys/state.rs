//! Shared per-circuit key-manager state. Composed by each per-circuit
//! manager (`PrimaryKeyManager`, `FallbackKeyManager`,
//! `LayerHashesKeyManager`, `EventKeyManager`) so that the mechanical
//! parts of the lifecycle — SRS load, cache-hit check, keygen
//! timing/logging, save-and-set, on-demand PK load/unload, accessor
//! panic messages — live in exactly one place. Per-circuit files only
//! carry their circuit-specific bits (witness construction inside
//! `ensure_keys`, degree constants, and any extra accessors).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, Circuit, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use tracing::{info, warn};

use super::{
    common::{
        config_path, load_config, load_srs, manifest_path, pk_path, read_manifest, save_config,
        save_manifest, save_pk, save_vk, sha256_file, try_load_pk, try_load_vk, vk_path,
        MANIFEST_FORMAT,
    },
    event::{EVENT_CIRCUIT_REVISION, PREFIX as EVENT_PREFIX},
};

/// How long to wait for another process's keygen before giving up.
///
/// The event circuit takes ~7 minutes; the primary and fallback circuits
/// are larger. 30 minutes is "somebody is genuinely doing the work" and
/// not "somebody wedged". A caller that hits the cap gets a refusal
/// naming the lock, which is better than blocking a withdrawal forever.
const KEYGEN_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// Poll interval while waiting. `flock` has no timed variant in libc, so
/// the wait is `LOCK_NB` in a loop.
const KEYGEN_LOCK_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Cross-process exclusion for one prefix's keygen.
///
/// `params_dir` is documented as shared with the bundle daemon
/// (`ackinacki-bridge/README.md`), and [`KeyManagerState::run_keygen`] had
/// nothing stopping two processes from running one at once. Per-file
/// writes are atomic, so that never tears a file — but the manifest is
/// written LAST and hashes whatever is on disk at that moment, so an
/// interleave where the two processes build DIFFERENT circuits publishes a
/// manifest that is internally consistent and describes a mixed keyset.
/// Every later check then passes: `new` matches the vk digest, the probe
/// matches the pk digest, the verdict is warm. `create_proof` takes the
/// proving key of one circuit while self-verification takes the verifying
/// key of the other, and the proof fails at stage 5 — after the burn.
///
/// (Two processes building the SAME circuit produce byte-identical keys,
/// so that interleave is harmless for content. It still doubles the ~3 GB
/// and the ~7 minutes, against a preflight that reserved for one.)
///
/// **`flock`, not a marker file.** A lock file created with `O_EXCL` and
/// removed on the way out is a deadlock the moment a process is killed
/// mid-keygen: the marker survives and nothing knows it is stale. `flock`
/// lives on the open descriptor, and the kernel drops it when the
/// descriptor closes — including on SIGKILL. So a lock that IS held proves
/// a live holder, which is what makes the timeout message below safe to
/// write as "find that process" rather than "maybe delete this file".
///
/// The lock file itself is created once and never removed. Unlinking it
/// would race: another process can be holding a lock on an inode whose
/// name is already gone, and the next arrival would create a fresh file
/// and lock that instead — two "exclusive" holders, which is the whole
/// failure this exists to prevent.
struct KeygenLock {
    /// Held open for the guard's lifetime; dropping it releases the lock.
    _file: std::fs::File,
    /// Whether this acquisition had to wait for somebody else.
    waited: bool,
}

impl KeygenLock {
    fn path(params_dir: &Path, prefix: &str) -> PathBuf {
        params_dir.join(format!("{prefix}_keygen.lock"))
    }

    /// Open the lock file and take the lock if it is free.
    ///
    /// `Ok(None)` is "somebody holds it" and `Err` is "the lock could not
    /// be attempted". Keeping those apart is the point: treating an EBADF
    /// or an EACCES as "busy" would spin for the full half hour and then
    /// report a contention that never existed.
    fn try_acquire(params_dir: &Path, prefix: &str) -> anyhow::Result<Option<std::fs::File>> {
        use std::os::unix::io::AsRawFd;

        let path = Self::path(params_dir, prefix);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open keygen lock {}", path.display()))?;

        // SAFETY: `file` outlives the call, so the descriptor is valid.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(file));
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            // A guard, not `Some(A) | Some(B)`: the two constants are the
            // same value on Linux, so as patterns the second is
            // unreachable and the compiler says so. Both are named because
            // POSIX allows them to differ, and this must keep meaning
            // "already held" on a target where they do.
            Some(n) if n == libc::EWOULDBLOCK || n == libc::EAGAIN => Ok(None),
            _ => Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }

    fn acquire(params_dir: &Path, prefix: &'static str) -> anyhow::Result<Self> {
        let path = Self::path(params_dir, prefix);
        if let Some(file) = Self::try_acquire(params_dir, prefix)? {
            return Ok(Self {
                _file: file,
                waited: false,
            });
        }

        warn!(
            lock = %path.display(),
            "another process is generating the {prefix} keys; waiting rather than writing over \
             them. Two keygens in one params_dir can publish a manifest that describes a mixed \
             keyset",
        );
        let started = std::time::Instant::now();
        let mut announced = std::time::Instant::now();
        loop {
            std::thread::sleep(KEYGEN_LOCK_POLL);
            if let Some(file) = Self::try_acquire(params_dir, prefix)? {
                info!(
                    waited_s = started.elapsed().as_secs(),
                    "acquired the {prefix} keygen lock",
                );
                return Ok(Self {
                    _file: file,
                    waited: true,
                });
            }
            if started.elapsed() >= KEYGEN_LOCK_WAIT {
                anyhow::bail!(
                    "waited {} minutes for the {prefix} keygen lock at {} and it is still held. A \
                     held flock means a LIVE process holds it — the kernel releases it when that \
                     process exits, so this is not a stale file and deleting it would let two \
                     keygens run at once. Find the other process (`fuser {}` or `lsof {}`) and \
                     let it finish, or stop it. Nothing was written.",
                    KEYGEN_LOCK_WAIT.as_secs() / 60,
                    path.display(),
                    path.display(),
                    path.display(),
                );
            }
            if announced.elapsed() >= std::time::Duration::from_secs(60) {
                announced = std::time::Instant::now();
                info!(
                    waited_s = started.elapsed().as_secs(),
                    "still waiting for the {prefix} keygen lock",
                );
            }
        }
    }
}

/// SRS + optional VK/PK/config cache with disk-backed lifecycle.
pub(crate) struct KeyManagerState {
    params_dir: PathBuf,
    prefix: &'static str,
    /// Circuit revision this manager's keys must declare, or `None` for
    /// managers that do not (yet) carry a manifest.
    ///
    /// `Option` rather than a bare `u32` because "no manifest expected" and
    /// "manifest must say 0" are different, and conflating them would make
    /// every un-migrated circuit regenerate on every run.
    expected_revision: Option<u32>,
    srs: ParamsKZG<Bn256>,
    k: u32,
    vk: Option<VerifyingKey<G1Affine>>,
    pk: Option<ProvingKey<G1Affine>>,
    config: Option<BaseCircuitParams>,
}

impl KeyManagerState {
    /// Construct: load SRS at `srs_k` (may exceed circuit `k` when the
    /// ceremony was oversized — see `LayerHashesKeyManager::KEYGEN_SRS_K`
    /// / `EventKeyManager::KEYGEN_SRS_K`), best-effort load any cached
    /// config/VK from disk; log if PK file is present (loaded on demand).
    ///
    /// `expected_revision` is a constructor parameter rather than a setter
    /// because a setter would make "constructed but not yet told its
    /// revision" a reachable state, and that state answers `keys_cached()`
    /// wrongly.
    pub(crate) fn new(
        params_dir: &Path,
        prefix: &'static str,
        k: u32,
        srs_k: u32,
        expected_revision: Option<u32>,
    ) -> Self {
        std::fs::create_dir_all(params_dir).ok();
        let srs = load_srs(params_dir, srs_k);
        let mut state = Self {
            params_dir: params_dir.to_path_buf(),
            prefix,
            expected_revision,
            srs,
            k,
            vk: None,
            pk: None,
            config: None,
        };
        state.install_cached_keys();
        state
    }

    /// Install cached keys when the manifest vouches for them, and report
    /// whether [`Self::keys_cached`] can now be true.
    ///
    /// Extracted from [`Self::new`] so [`Self::run_keygen`] can ask the
    /// same question after waiting on the keygen lock: whoever held that
    /// lock was writing these exact four files, and re-deriving ~3 GB over
    /// ~7 minutes to reproduce what they have just published is time the
    /// CLI spends AFTER the burn. One implementation, so the constructor's
    /// answer and the post-wait answer cannot drift.
    fn install_cached_keys(&mut self) -> bool {
        let prefix = self.prefix;
        let Ok(config) = load_config(&self.params_dir, prefix) else {
            return false;
        };
        // Refuse to install a verifying key from another circuit
        // revision. This is the ONLY enforcement point that protects
        // consumers which never call `ensure_keys` —
        // `bridge-verifier-daemon` reads `vk_opt()` directly and would
        // otherwise verify against a stale key without any complaint.
        //
        // `None` here means "this circuit does not version its keys",
        // which is the honest state of primary, fallback and layer
        // today; they are unaffected.
        //
        // Revision AND the two small digests. The revision alone leaves
        // a grafted key installed: replace `event_vk.bin` with a
        // different — but valid — verifying key, leave the manifest
        // saying the current revision, and this gate would wave it
        // through to `bridge-verifier-daemon`, which then verifies with
        // the wrong key. The CLI's probe catches that case; the daemon
        // never runs the CLI's probe.
        //
        // The proving key is deliberately NOT hashed here. It is
        // ~2.65 GB, this runs in every constructor for all four
        // managers, and the daemon does not load a PK at all. The VK
        // and config are megabytes and kilobytes; the PK's digest stays
        // in `probe_event_key_cache`, where the CLI pays for it once.
        let keys_ok = match self.expected_revision {
            None => true,
            // `.ok().flatten()` — this constructor deliberately
            // collapses `read_manifest`'s two failures back into one
            // answer. Absent and unreadable both mean "nothing here can
            // be trusted", and the action is identical: do not install
            // the key.
            //
            // Choosing to collapse them is not the same as being unable
            // to tell them apart, which is why `read_manifest` returns
            // `Result<Option<_>>` regardless. The CLI probe reports to a
            // human and keeps the two messages distinct; this runs
            // inside four constructors, in a daemon as often as in the
            // CLI, and has one thing to say.
            Some(want) => read_manifest(&self.params_dir, prefix)
                .ok()
                .flatten()
                .is_some_and(|m| {
                    // The format comes first here too.
                    // `deny_unknown_fields` stops a manifest that ADDS a
                    // field from parsing at all; this stops one that
                    // reuses these fields differently from being read as
                    // if it did not.
                    m.manifest_format == MANIFEST_FORMAT
                        && m.circuit_revision == want
                        && sha256_file(&vk_path(&self.params_dir, prefix))
                            .is_ok_and(|d| d == m.vk_sha256)
                        && sha256_file(&config_path(&self.params_dir, prefix))
                            .is_ok_and(|d| d == m.config_sha256)
                }),
        };
        if !keys_ok {
            // Loud, and specific. The daemon's own message says "VK not
            // found", which would send an operator looking for a missing
            // file rather than a stale one.
            //
            // The manifest clause is not padding: this branch is also
            // where an unreadable manifest lands, and without it the
            // message names three causes that all involve the keys and
            // none that involves the record of them.
            warn!(
                params_dir = %self.params_dir.display(),
                prefix,
                "cached {prefix} keys do not match their manifest, or the manifest could not \
                 be read (wrong circuit revision; a manifest format this build does not know; \
                 the verifying key or config was replaced; the manifest is unparseable or \
                 absent); ignoring them. Re-run the prover to regenerate.",
            );
            return false;
        }

        info!("found {} config: {:?}", prefix, config);
        if let Some(vk) = try_load_vk(&self.params_dir, prefix, &config) {
            info!("loaded {} VK from cache", prefix);
            self.vk = Some(vk);
        }
        if pk_path(&self.params_dir, prefix).exists() {
            info!("{} PK found on disk (will load on demand)", prefix);
        }
        self.config = Some(config);
        // Deliberately `keys_cached()` and not `self.vk.is_some()`: the
        // caller's question is "can regeneration be skipped?", and that
        // predicate is the one that answers it everywhere else.
        self.keys_cached()
    }

    /// True iff `ensure_keys` can skip regeneration: VK in memory and PK on
    /// disk.
    ///
    /// **Unchanged from before the manifest landed**, and deliberately so.
    /// The manifest comparison belongs in `new`, because `new` is what
    /// `bridge-verifier-daemon` goes through and `keys_cached` is not. With
    /// the check there, a stale or unverifiable cache never gets a VK
    /// installed, so `self.vk` is `None` and this returns `false` without
    /// knowing why — which is what makes the CLI's `Cold` report true rather
    /// than a prediction of a keygen that never happens.
    ///
    /// It still does not verify the digests. That check costs ~2.65 GB of
    /// hashing and belongs in `probe_event_key_cache`, which the CLI runs
    /// once in stage 1; a constructor that four managers call cannot pay for
    /// it.
    ///
    /// The states this predicate still gets wrong are exactly the ones the
    /// probe refuses, and all three share a shape: the VK and config are
    /// fine, so `new` installs the VK, and the proving key — which this
    /// predicate only tests the *existence* of — is not:
    ///
    /// - a proving key present but not deserialisable (`Corrupt`);
    /// - a proving key whose digest disagrees with the manifest (`Corrupt`);
    /// - a directory at `event_pk.bin`, for which `Path::exists` answers
    ///   `true` (`Blocked`).
    ///
    /// A manifest that will not parse is deliberately NOT in that list, and
    /// this predicate does not get it wrong: `new` refuses to install the VK
    /// without a manifest it can read, so `self.vk` is `None` and this
    /// returns `false`. The probe agrees — `Cold` — and the run regenerates.
    /// Stage 1 turns all three above into an exit-2 refusal, so nothing
    /// reaches the runtime in a state where the two disagree.
    pub(crate) fn keys_cached(&self) -> bool {
        self.vk.is_some() && pk_path(&self.params_dir, self.prefix).exists()
    }

    /// Remove this prefix's manifest and make the removal durable.
    ///
    /// Called before `run_keygen` replaces any key file. Atomic per-file
    /// writes make each file whole or absent; they say nothing about the
    /// SET. Regenerating over an existing cache — what a revision bump or a
    /// format change produces — replaces the keys one at a time while the
    /// previous manifest still describes the previous keys. Die between
    /// `save_pk` and `save_manifest` and the next run finds a manifest whose
    /// digests match nothing: `Corrupt`, and a refusal needing a manual
    /// `--repair`, for a crash that should have read as cold.
    ///
    /// Retracting first inverts that. The window becomes "no manifest",
    /// which reads as `Cold`, which regenerates unattended.
    ///
    /// A no-op for managers with no `expected_revision`.
    pub(crate) fn retract_manifest(&self) -> anyhow::Result<()> {
        if self.expected_revision.is_none() {
            return Ok(());
        }
        let path = manifest_path(&self.params_dir, self.prefix);
        match std::fs::remove_file(&path) {
            Ok(()) => {},
            // Already absent is the desired state, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // Anything else — read-only mount, permissions — must stop the
            // keygen. Proceeding would write keys under a manifest that
            // still describes the old ones.
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("retract stale key manifest {}", path.display()))
            },
        }
        // The unlink is not durable until the directory entry is.
        std::fs::File::open(&self.params_dir)
            .and_then(|d| d.sync_all())
            .with_context(|| {
                format!(
                    "fsync {} after retracting the manifest",
                    self.params_dir.display()
                )
            })?;
        Ok(())
    }

    /// Run keygen_vk + keygen_pk with prefix-labelled timing/logging, then
    /// persist VK + PK + config to disk and install VK + config into `self`.
    /// The PK is intentionally dropped after save (freeing ~2.8–3.7 GB for
    /// the BLS circuits); callers reload on demand via [`Self::load_pk`].
    pub(crate) fn run_keygen<C>(
        &mut self,
        circuit: &C,
        base_params: BaseCircuitParams,
    ) -> anyhow::Result<()>
    where
        C: Circuit<Fr>,
    {
        // FIRST of all — before the manifest is retracted, let alone
        // before a key file is written. Everything below is a sequence of
        // individually-atomic writes that is not atomic as a SET, and this
        // is what stops a second process interleaving with it.
        let lock = KeygenLock::acquire(&self.params_dir, self.prefix)?;

        // We may have just waited minutes for that lock. Whoever held it
        // was writing these exact four files, so ask whether they are now
        // good before spending another ~7 minutes and ~3 GB reproducing
        // them — time the CLI spends after the burn.
        //
        // Only when we actually waited: on the uncontended path the
        // caller has already decided regeneration is needed, and re-asking
        // would re-read the config on every keygen for nothing.
        if lock.waited && self.install_cached_keys() {
            info!(
                "{} keys were generated by the process we waited for; adopting them",
                self.prefix
            );
            return Ok(());
        }

        // FIRST — before any key file is touched.
        self.retract_manifest()?;
        // The ordering is the whole crash-consistency argument and no unit
        // test can observe it: separating "retracted" from "saved" needs a
        // crash between them. Assert it here instead, where it costs
        // nothing in release and fails loudly in test builds if someone
        // moves this call below the saves.
        debug_assert!(
            self.expected_revision.is_none()
                || !manifest_path(&self.params_dir, self.prefix).exists(),
            "run_keygen must retract the manifest before writing any key file",
        );

        info!("{} base_circuit_params: {:?}", self.prefix, base_params);

        let t = Instant::now();
        let vk = keygen_vk(&self.srs, circuit)
            .with_context(|| format!("{} keygen_vk failed", self.prefix))?;
        info!("{} keygen_vk: {:?}", self.prefix, t.elapsed());

        let t = Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), circuit)
            .with_context(|| format!("{} keygen_pk failed", self.prefix))?;
        info!("{} keygen_pk: {:?}", self.prefix, t.elapsed());

        save_vk(&self.params_dir, self.prefix, &vk)?;
        save_pk(&self.params_dir, self.prefix, &pk)?;
        save_config(&self.params_dir, self.prefix, &base_params)?;

        // LAST, and only for managers that declare a revision. `run_keygen`
        // serves all four, so an unconditional `EVENT_CIRCUIT_REVISION`
        // here would stamp the event circuit's number onto primary,
        // fallback and layer keys — a lie in three files, and one that the
        // revision gate in `new` would then start believing the moment any
        // of them grows an `expected_revision`.
        if let Some(revision) = self.expected_revision {
            // After all three saves, so the digests describe what is on
            // disk.
            save_manifest(&self.params_dir, self.prefix, revision)?;
        }

        self.vk = Some(vk);
        self.config = Some(base_params);
        // `pk` drops here — memory freed; reload on demand via `load_pk`.
        Ok(())
    }

    /// Load PK from disk into memory. Idempotent — no-op if already loaded.
    /// Requires `ensure_keys` (or a cached config on disk) to have populated
    /// `self.config` first.
    pub(crate) fn load_pk(&mut self) -> anyhow::Result<()> {
        if self.pk.is_some() {
            return Ok(());
        }
        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::format_err!("{} config not loaded — run ensure_keys first", self.prefix)
        })?;
        let path = pk_path(&self.params_dir, self.prefix);
        match std::fs::metadata(&path).ok().map(|m| m.len()) {
            Some(bytes) => info!(
                "loading {} PK ({}) from disk...",
                self.prefix,
                format_bytes(bytes)
            ),
            None => info!("loading {} PK from disk...", self.prefix),
        }
        let t = Instant::now();
        let pk = try_load_pk(&self.params_dir, self.prefix, config).ok_or_else(|| {
            anyhow::format_err!("failed to load {} PK from {}", self.prefix, path.display())
        })?;
        info!("{} PK loaded in {:?}", self.prefix, t.elapsed());
        self.pk = Some(pk);
        Ok(())
    }

    /// Drop PK from memory. Cheap idempotent no-op if already unloaded.
    pub(crate) fn unload_pk(&mut self) {
        if self.pk.is_some() {
            self.pk = None;
            info!("{} PK unloaded from memory", self.prefix);
        }
    }

    // ---- accessors ----

    pub(crate) fn params_dir(&self) -> &Path {
        &self.params_dir
    }
    pub(crate) fn srs(&self) -> &ParamsKZG<Bn256> {
        &self.srs
    }
    pub(crate) fn k(&self) -> u32 {
        self.k
    }
    pub(crate) fn vk_opt(&self) -> Option<&VerifyingKey<G1Affine>> {
        self.vk.as_ref()
    }
    pub(crate) fn vk(&self) -> &VerifyingKey<G1Affine> {
        self.vk
            .as_ref()
            .unwrap_or_else(|| panic!("{} VK not loaded", self.prefix))
    }
    pub(crate) fn pk(&self) -> &ProvingKey<G1Affine> {
        self.pk
            .as_ref()
            .unwrap_or_else(|| panic!("{} PK not loaded", self.prefix))
    }
    pub(crate) fn config(&self) -> &BaseCircuitParams {
        self.config
            .as_ref()
            .unwrap_or_else(|| panic!("{} config not loaded", self.prefix))
    }
}

/// What [`KeyManagerState::keys_cached`] will conclude about a
/// `params_dir`, and why.
///
/// **Where each on-disk state lands.** The terms are easy to use loosely —
/// "damaged manifest" alone can mean three different things — so this
/// table, not the prose, is the contract:
///
/// | on disk | verdict | who fixes it |
/// |---|---|---|
/// | no manifest | `Cold` — predates the mechanism | the next run |
/// | manifest is not valid JSON, or its shape is unknown | `Cold` — no record, so no claim to contradict | the next run |
/// | valid manifest, different `circuit_revision` or `manifest_format` | `Cold` — a self-consistent cache belonging to another build | the next run |
/// | valid manifest, digest disagrees with a file | `Corrupt` — contradiction | `--repair` |
/// | proving key present but not deserialisable | `Corrupt` — contradiction | `--repair` |
/// | any of the four key paths is a directory | `Blocked` | a human |
///
/// **The dividing line between `Cold` and the two refusals is not "how bad
/// is it" but "can the run fix it".** Regeneration overwrites all four
/// files, so it would resolve a `Corrupt` cache too — except that
/// regenerating is exactly what those states make unsafe to *assume*, since
/// `keys_cached()` only tests that the PK exists and would skip the keygen
/// this probe just promised.
///
/// The unparseable-manifest row also has to fit the REMEDY, which is why it
/// is `Cold` and not a refusal: `Cold` turns the `params_dir` writability
/// probe on and the refusals turn it off, and the remedy for an unparseable
/// manifest is writing a new one. Refusing there would send an operator to
/// `--repair` for a state the next run fixes unattended, and would suppress
/// the permissions check that is the one thing likely to stop it.
#[derive(Debug)]
pub enum KeyCacheState {
    /// The keys are the ones keygen wrote, for this circuit revision, and
    /// this build can read them. `keys_cached()` will be true; keygen is
    /// skipped and no headroom is needed.
    Warm,
    /// There is no usable cache. `keys_cached()` will be false too, so the
    /// run regenerates: ~3 GB and ~7 min.
    Cold { why: String },
    /// **The cache contradicts itself: refuse the run, and `--repair` fixes
    /// it.** A digest that does not match its file, a proving key that will
    /// not deserialise.
    ///
    /// Deliberately *not* a prediction about `keys_cached()`. It is true for
    /// some of these states (a corrupt proving key is a file that exists, so
    /// the manager would skip keygen and die at `load_pk`) and false for
    /// others (a truncated verifying key fails to load, so the manager would
    /// regenerate quite happily). Tying the variant's meaning to one of
    /// those would be wrong, and the wrongness would be invisible because
    /// both still deserve a refusal: a cache whose own records disagree with
    /// its contents is not one to spend a burn on, whichever way the runtime
    /// would jump.
    Corrupt { why: String },
    /// **Something is in the way, and no tool here will move it:** one of
    /// the four key paths is a directory. Refuse the run, and send the
    /// operator — not `--repair` — at it.
    ///
    /// Separate from [`Corrupt`](Self::Corrupt) rather than a `repairable:
    /// bool` inside it, because the difference is in what every consumer
    /// must *say*, and there are four of them: `keygen_requirement`'s
    /// refusal text, `probe_event_keys`'s "pass --repair" line, the
    /// `--repair` path itself, and the runbook. A flag inside `Corrupt`
    /// leaves each free to forget the branch — preflight promising "clearing
    /// it is safe, run --repair", the tool answering "pass --repair to clear
    /// it", and `--repair` then refusing. A variant makes the compiler
    /// enumerate them.
    ///
    /// Same exit code as `Corrupt` (2, `PreflightRefused`) — this is a
    /// message-level distinction, not a contract change.
    Blocked { why: String },
}

/// Answer, without constructing a [`KeyManagerState`], what the event key
/// manager will do with `params_dir`.
///
/// Deliberately does **not** call `KeyManagerState::new`: that loads the
/// ~464 MB SRS, and a preflight asking "will keygen run?" should not pay for
/// the ceremony to find out.
///
/// Mirrors `new` + `keys_cached` step for step — config, then VK against
/// that config, then the PK — and then goes two further: it hashes the PK,
/// and it deserialises it. `keys_cached()` only tests that the PK *exists*,
/// so an empty, truncated or format-shifted file reads as a cache hit and
/// fails at proof time, after the burn.
///
/// # Cost
///
/// State it exactly. On a warm cache the ~2.65 GB proving key is read
/// **three** times per run:
///
/// 1. here, streamed by `sha256_file` to verify its digest;
/// 2. here, by `try_load_pk`, which reads it again and deserialises it;
/// 3. in stage 5, by `EventKeyManager::load_pk`, which reads and deserialises
///    it a third time.
///
/// So ~8 GB of sequential reads and two full deserialisations, of which only
/// the third was happening before. Cold page cache and a slow disk put that
/// in the tens of seconds; a host with room to keep 2.65 GB cached pays
/// mostly for the two deserialisations. Peak RSS in preflight rises to
/// roughly the in-memory size of the proving key — the same peak stage 5
/// reaches — and is released before the burn.
///
/// Why not fewer passes:
///
/// - **Fuse 1 and 2** (hash while deserialising, check the digest after):
///   defeats the ordering the whole design rests on. Both halo2 readers
///   `unwrap()` on malformed input, so the digest has to clear the bytes
///   *before* a reader sees them. Fusing would hand unverified bytes to a
///   panicking reader and learn the digest was wrong afterwards.
/// - **Reuse the key from 2 in stage 5**: means holding ~2.65 GB resident
///   across the burn, the capture wait and the resurrect/coverage wait — an
///   unbounded window, on a process whose whole design is to survive being
///   slow. Trading a bounded 8 GB of I/O for an unbounded 2.65 GB of RSS is the
///   wrong direction.
/// - **Drop pass 2**: possible, and the only real option, but it gives up the
///   one failure the digest cannot see — a byte-identical file this build can
///   no longer parse, which is what a halo2 bump without a revision bump
///   produces. That failure lands in stage 5, after the burn. Keeping it is a
///   deliberate trade.
pub fn probe_event_key_cache(params_dir: &Path) -> KeyCacheState {
    let prefix = EVENT_PREFIX;

    // ---- First: can these four names be REPLACED at all? ----
    //
    // Every branch below answers "should keygen run?", and each one that
    // says yes carries an unstated second claim: that keygen will then be
    // able to do its work. That claim has a hole, and it is the expensive
    // kind — `Cold` is a promise the run regenerates unattended, redeemed in
    // stage 5, after the burn.
    //
    // Regeneration is `remove_file` on the manifest (`retract_manifest`)
    // followed by four renames onto these names. A directory at any of them
    // fails all of that with EISDIR, and `Cold` would have promised
    // otherwise. `mkdir` at one of these paths is not an exotic accident:
    // `docker run -v $HOST/event_pk.bin:/params/event_pk.bin` creates the
    // mount point as a DIRECTORY when the host path does not exist, and an
    // interrupted `rsync -a` can leave one behind too.
    //
    // `Blocked`, its own variant: `Cold` means the run fixes it, `Corrupt`
    // means `--repair` fixes it, and this one means neither can.
    for path in [
        config_path(params_dir, prefix),
        vk_path(params_dir, prefix),
        pk_path(params_dir, prefix),
        manifest_path(params_dir, prefix),
    ] {
        // `symlink_metadata`, NOT `metadata`, and `is_dir()`, NOT
        // `!is_file()`. Both of the other choices refuse runs that would
        // have worked:
        //
        //  - `metadata` follows the final symlink, so a symlink POINTING AT a directory
        //    reports as a directory. Neither `unlink` nor `rename` follows the final
        //    component (POSIX), so both operate on the link itself and replace it
        //    happily. `symlink_metadata` asks about the entry, which is the thing being
        //    replaced.
        //  - `!is_file()` would also sweep in FIFOs, sockets and device nodes. Those
        //    are ordinary directory entries: `unlink` removes them, `rename` replaces
        //    them.
        //
        // What is left is the single entry type that genuinely blocks both
        // operations: a real directory. `unlink` on one is EISDIR and
        // `rename` over one is EISDIR/ENOTDIR, so keygen cannot proceed
        // however patient it is.
        //
        // `is_ok_and` folds the `Err` cases in as "nothing in the way",
        // which is right for all of them: absent, dangling symlink, or a
        // `params_dir` this process cannot even stat — and that last one
        // fails at `load_config` two lines below anyway.
        if std::fs::symlink_metadata(&path).is_ok_and(|md| md.is_dir()) {
            return KeyCacheState::Blocked {
                why: format!(
                    "{} is a directory. Keygen replaces these four names by rename, which cannot \
                     write through one, so nothing this tool does will help: remove it by hand. \
                     (`--repair` deliberately will not — deleting a directory nobody asked it to \
                     delete is not a repair.) A bind mount whose host path does not exist is the \
                     usual cause",
                    path.display(),
                ),
            };
        }
    }

    let Ok(config) = load_config(params_dir, prefix) else {
        return KeyCacheState::Cold {
            why: format!("no readable {}", config_path(params_dir, prefix).display()),
        };
    };
    // Presence only — deserialising here would be the same panic hazard as
    // the proving key, and the digest check below is strictly stronger.
    if !vk_path(params_dir, prefix).is_file() {
        return KeyCacheState::Cold {
            why: format!("no {}", vk_path(params_dir, prefix).display()),
        };
    }
    let pk = pk_path(params_dir, prefix);
    if !pk.exists() {
        return KeyCacheState::Cold {
            why: format!("no {}", pk.display()),
        };
    }

    // ---- Nothing is deserialised until the bytes have been verified. ----
    //
    // halo2's readers do not return errors on malformed input, they PANIC.
    // `Polynomial::read` is `read_exact(..).unwrap()` plus
    // `F::read(..).unwrap()` (`halo2-axiom/src/poly.rs:169-176`), and the
    // verifying key is no safer: `SERDE_FMT` is `RawBytesUnchecked`, whose
    // `read_raw_unchecked` is another `read_exact(..).unwrap()`
    // (`halo2curves/src/derive/field.rs:350`). Both are wrapped in
    // `catch_unwind` in `common.rs`, so they report rather than abort — but
    // only after the digests have already cleared the bytes.
    //
    // `RawBytesUnchecked` also skips the canonical/on-curve checks, which
    // means the opposite failure exists too: a byte flipped inside the
    // proving key, past the embedded verifying key, deserialises happily,
    // keeps its length, and leaves `transcript_repr()` — a digest of the VK
    // only — unchanged. Warm, wrong, and the proof fails after the burn.
    //
    // One mechanism answers most of it: record a digest of each file at
    // keygen time and verify it by streaming the bytes. Streaming cannot
    // panic, catches truncation and same-length corruption alike, and —
    // because a grafted verifying key is just a file whose digest does not
    // match — it subsumes the pk/vk consistency comparison as well.
    //
    // Three outcomes, not two. `read_manifest` returns
    // `anyhow::Result<Option<KeyManifest>>` so that this call site can tell
    // "there is no manifest" from "there is one and it cannot be read".
    // Folding both into an `Option` is what would print "it predates the
    // check" over a truncated file, and report an EACCES manifest as a
    // benign pre-upgrade cache.
    //
    // Both are `Cold` — see the table on [`KeyCacheState`]. They differ in
    // what the operator is told, and in the `warn!`: within this design
    // nothing legitimate produces the second one. `save_manifest` renames a
    // complete temp file into place, so an interrupted keygen leaves no
    // manifest rather than half of one; an unreadable manifest means
    // external damage, a hand edit, permissions, or a format this build does
    // not know.
    //
    // Neither arm has touched a halo2 reader, which is the property the
    // digest-first ordering exists to preserve.
    let m = match read_manifest(params_dir, prefix) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return KeyCacheState::Cold {
                why: format!(
                    "{} has no revision manifest — it predates the check, so this build cannot \
                     confirm the keys are for its circuit. Regenerating.",
                    params_dir.display(),
                ),
            };
        },
        Err(e) => {
            warn!(
                path = %manifest_path(params_dir, prefix).display(),
                error = %e,
                "the key manifest exists but could not be read; treating the cache as cold and \
                 regenerating. No writer in this crate produces a partial manifest, so this is \
                 external damage, permissions, or a format this build does not know.",
            );
            return KeyCacheState::Cold {
                // `{e:#}`, not `{e}`: `anyhow`'s plain Display prints only
                // the outermost context ("parse …/event_manifest.json") and
                // drops the serde message underneath it, which is the half
                // that says *what* is wrong with the file.
                why: format!(
                    "{} exists but could not be read ({e:#}) — this build cannot confirm which \
                     circuit the keys belong to. Regenerating.",
                    manifest_path(params_dir, prefix).display(),
                ),
            };
        },
    };
    // Format before revision. A manifest whose shape this build does not
    // understand says nothing it can act on, including about the revision —
    // reading `circuit_revision` out of it first would be trusting one field
    // of a record whose meaning is in question.
    //
    // `Cold`, like a revision mismatch and for the same reason: it is a
    // self-consistent cache belonging to a different build, and this run
    // fixes it by regenerating.
    if m.manifest_format != MANIFEST_FORMAT {
        return KeyCacheState::Cold {
            why: format!(
                "{} is manifest format {} but this build writes and reads format \
                 {MANIFEST_FORMAT} — the record describing these keys is not one this build can \
                 act on. Regenerating.",
                manifest_path(params_dir, prefix).display(),
                m.manifest_format,
            ),
        };
    }
    if m.circuit_revision != EVENT_CIRCUIT_REVISION {
        return KeyCacheState::Cold {
            why: format!(
                "{} was generated for circuit revision {} but this build is revision \
                 {EVENT_CIRCUIT_REVISION} — the keys are self-consistent and belong to a \
                 different circuit. Regenerating.",
                manifest_path(params_dir, prefix).display(),
                m.circuit_revision,
            ),
        };
    }
    // Verify all three files against the digests recorded at keygen.
    //
    // This is what makes a grafted verifying key visible: the two keysets in
    // that scenario are each internally valid, and the graft shows up simply
    // as a file whose bytes are not the ones this manifest describes.
    // `create_proof` takes `event_km.pk()` while self-verification takes
    // `event_km.vk()` from the separate file, so a mismatched pair produces
    // a proof that fails its own verification — in stage 5, after the burn.
    //
    // The manifest is a file on disk, so its fields are untrusted input —
    // hand-edited, half-written, or from a future format. Validate the shape
    // before slicing: `&want[..8]` on a short or non-ASCII string panics,
    // and panicking inside the function whose job is to diagnose a broken
    // cache is the same mistake as the reader panics above.
    let digests = [
        (pk.clone(), m.pk_sha256.as_str(), "proving key"),
        (
            vk_path(params_dir, prefix),
            m.vk_sha256.as_str(),
            "verifying key",
        ),
        (
            config_path(params_dir, prefix),
            m.config_sha256.as_str(),
            "config",
        ),
    ];
    if let Some((_, bad, what)) = digests
        .iter()
        .find(|(_, d, _)| d.len() != 64 || !d.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        // `Corrupt`, deliberately, and NOT `Cold`.
        //
        // `Cold` is a promise that the run will regenerate — and the runtime
        // would not: `keys_cached()` checks the revision via `new`, not the
        // digests, so with a current revision, a malformed pk digest and a
        // truncated proving key it answers `true`, skips keygen, and dies in
        // `load_pk` after the burn. Preflight predicting a keygen the
        // runtime then skips is the precise defect the manifest exists to
        // remove; reintroducing it one branch over would be worse than not
        // checking at all.
        //
        // A *missing* manifest stays `Cold` — that is an old cache with
        // intact files and regeneration really is the remedy. A manifest
        // that exists and is malformed is a cache someone should look at.
        return KeyCacheState::Corrupt {
            why: format!(
                "{}: the {what} digest is not 64 hex characters ({} bytes) — the manifest is \
                 damaged, so nothing here can be vouched for",
                manifest_path(params_dir, prefix).display(),
                bad.len(),
            ),
        };
    }

    for (path, want, what) in digests {
        let got = match sha256_file(&path) {
            Ok(d) => d,
            Err(e) => {
                return KeyCacheState::Corrupt {
                    why: format!("{} could not be read: {e}", path.display()),
                }
            },
        };
        if got != want {
            let len = std::fs::metadata(&path).map(|md| md.len()).unwrap_or(0);
            return KeyCacheState::Corrupt {
                why: format!(
                    "{} does not match the digest recorded when these keys were generated — the \
                     {what} is truncated, corrupted, or was copied in from a different keyset \
                     ({len} bytes, sha256 {}… vs {}…)",
                    path.display(),
                    &got[..8],
                    &want[..8],
                ),
            };
        }
    }

    // Digests prove the bytes are the ones keygen wrote. They do not prove
    // that THIS binary can still read them: a halo2 layout change leaves the
    // file byte-identical and no longer loadable, and the failure would land
    // in stage 5 again.
    //
    // So load both. Loading only the verifying key would not do: `SERDE_FMT`
    // governs how individual field and curve elements are encoded, not the
    // layout of the structures around them. `ProvingKey::read` reads the VK
    // and then goes on to `l0`, `l_last`, `l_active_row`, two polynomial
    // vectors and the permutation key (`halo2-axiom/src/plonk.rs:388-400`) —
    // all PK-only, all free to change while the VK still reads perfectly.
    //
    // The two arms end differently, and the asymmetry is not an oversight.
    //
    // A verifying key that will not load is `Cold`, and that is truthful:
    // `keys_cached()` tests `self.vk.is_some()`, and `self.vk` is whatever
    // `try_load_vk` returned in `KeyManagerState::new`. Fail here and the
    // runtime fails there too — it really will regenerate.
    //
    // A proving key that will not load is `Corrupt`. `keys_cached()` tests
    // `pk_path().exists()`, nothing more, so a PK that is present and
    // unreadable answers `true`: the runtime would skip the keygen this
    // probe just promised and die in `load_pk` after the burn.
    if try_load_vk(params_dir, prefix, &config).is_none() {
        return KeyCacheState::Cold {
            why: format!(
                "{} matches its recorded digest but this build cannot deserialise it — the key \
                 format changed under a cache that is otherwise intact. Regenerating.",
                vk_path(params_dir, prefix).display(),
            ),
        };
    }
    if try_load_pk(params_dir, prefix, &config).is_none() {
        return KeyCacheState::Corrupt {
            why: format!(
                "{} matches its recorded digest but this build cannot deserialise the proving key \
                 — the proving-key layout changed under a cache that is otherwise intact. The \
                 runtime would not notice (it only checks that the file exists), so this must be \
                 cleared rather than waited out",
                pk.display(),
            ),
        };
    }

    KeyCacheState::Warm
}
/// Format a byte count with a binary IEC unit, one line for the
/// `load_pk` operator hint. Auto-picks GiB / MiB / KiB so a large BLS
/// PK reads as "3.62 GiB" while a smaller Circuit-4 PK reads as
/// "412.7 MiB" instead of "0.40 GiB".
/// The proving key's recorded digest and the file it describes, for a
/// caller that wants to re-verify the key LATER.
///
/// [`probe_event_key_cache`] streams ~2.65 GB to reach a `Warm` verdict and
/// nothing repeats that: the runtime gate is `keys_cached()`, which asks
/// only whether the file exists, and [`KeyManagerState::new`] deliberately
/// skips the proving key's digest. For the withdraw CLI that one check sits
/// before the irreversible burn and up to ~91 minutes of anchor wait, and
/// the key is not read until after both.
///
/// Returns the digest the MANIFEST records, which a `Warm` verdict has just
/// confirmed the file matches. The caller keeps that string and compares a
/// fresh [`sha256_of`] against it — deliberately not against the manifest
/// read again later, so replacing the key and its manifest together does
/// not pass.
///
/// `None` when there is no readable manifest, which is every state except
/// warm; there is nothing to vouch for then.
pub fn event_pk_recorded_digest(params_dir: &Path) -> Option<(PathBuf, String)> {
    let m = read_manifest(params_dir, EVENT_PREFIX).ok().flatten()?;
    Some((pk_path(params_dir, EVENT_PREFIX), m.pk_sha256))
}

/// Stream a file and return its SHA-256, lower-case hex.
///
/// Streaming, so it cannot be fooled by a truncation and cannot panic on
/// malformed content the way halo2's readers do — the same reason
/// [`probe_event_key_cache`] hashes before it deserialises.
pub fn sha256_of(path: &Path) -> std::io::Result<String> {
    sha256_file(path)
}

/// Human-readable byte count. Public because `keys` re-exports it: the
/// probe binary reports leaked temp sizes, and its neighbouring output
/// ("loading event PK (2.65 GiB)") is already in these units.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- The keygen lock -------------------------------------------------
    //
    // `run_keygen` is the write it protects, and running one costs ~7
    // minutes and ~3 GB, so these exercise the guard directly. What has to
    // hold: it excludes, it excludes ACROSS PROCESSES (a thread-only lock
    // would pass a same-process test and fail the case that matters, since
    // `params_dir` is shared with the bundle daemon), it does not exclude
    // two different circuits from each other, and a holder that dies
    // releases it.

    #[test]
    fn a_second_acquisition_in_this_process_is_refused_while_the_first_lives() {
        let d = tempfile::TempDir::new().unwrap();
        let first = KeygenLock::acquire(d.path(), "event").expect("uncontended");
        assert!(!first.waited, "nothing to wait for");

        // `flock` is per open-file-description, not per process, so a
        // second `open` here is exactly what a second process does.
        let path = KeygenLock::path(d.path(), "event");
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        use std::os::unix::io::AsRawFd;
        // SAFETY: `f` is live for the call.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, -1, "the lock must be held");
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap();
        assert!(
            errno == libc::EWOULDBLOCK || errno == libc::EAGAIN,
            "held, not broken: errno {errno}",
        );

        // And released on drop, so the next keygen is not blocked forever.
        drop(first);
        // SAFETY: as above.
        assert_eq!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "dropping the guard must release the lock",
        );
    }

    #[test]
    fn two_circuits_do_not_block_each_other() {
        // Per prefix, not per directory. The four managers write disjoint
        // file sets, and serialising them would turn four independent
        // keygens into one queue for no correctness gain.
        let d = tempfile::TempDir::new().unwrap();
        let _event = KeygenLock::acquire(d.path(), "event").expect("event");
        let _primary = KeygenLock::acquire(d.path(), "primary").expect("primary must not block");
        assert_ne!(
            KeygenLock::path(d.path(), "event"),
            KeygenLock::path(d.path(), "primary"),
        );
    }

    #[test]
    fn a_lock_held_by_a_dead_process_is_free() {
        // The reason this is `flock` and not an `O_EXCL` marker file. A
        // keygen runs for minutes; killing one mid-write is ordinary. A
        // marker would survive and deadlock every later run until somebody
        // knew to delete it — and "delete this file if you think it is
        // stale" is advice that, followed wrongly, lets two keygens run at
        // once, which is the thing being prevented.
        let d = tempfile::TempDir::new().unwrap();
        let path = KeygenLock::path(d.path(), "event");

        // A child process that takes the lock and is then killed. `sh`
        // holds it on fd 9 and sleeps.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                // `exec sleep`, not `sleep`: a plain `sleep` is forked, and
                // `child.kill()` would then kill the shell while the forked
                // sleep kept the inherited descriptor — and the lock with
                // it. `exec` replaces the shell, so the PID we kill IS the
                // holder.
                "exec 9>'{}'; flock -x 9 || exit 1; echo ready; exec sleep 300",
                path.display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("sh must be runnable");

        // Wait for it to actually hold the lock before asserting on it.
        use std::io::Read;
        let mut out = child.stdout.take().unwrap();
        let mut buf = [0u8; 6];
        if out.read_exact(&mut buf).is_err() {
            // No `flock(1)` on this host: nothing to assert, and inventing
            // a pass would be worse than skipping.
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        assert!(
            KeygenLock::try_acquire(d.path(), "event")
                .unwrap()
                .is_none(),
            "the child holds it",
        );

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            KeygenLock::try_acquire(d.path(), "event")
                .unwrap()
                .is_some(),
            "the kernel releases a flock when its holder dies",
        );
    }

    #[test]
    fn format_bytes_picks_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2 * 1024), "2.0 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 + 512 * 1024), "3.5 MiB");
        assert_eq!(format_bytes(3_900_000_000), "3.63 GiB");
    }
}

#[cfg(test)]
mod probe_tests {
    use tempfile::TempDir;

    use super::{super::event::EventKeyManager, *};

    #[test]
    fn an_empty_dir_is_cold() {
        let d = TempDir::new().unwrap();
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Cold { .. }
        ));
    }

    #[test]
    fn a_pk_without_its_config_is_cold_not_warm() {
        // The exact false-positive the naive `event_pk.bin.is_file()` check
        // produced: keygen WILL run here, and a caller told "warm" skips the
        // headroom check that would have caught a full disk.
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 4096]).unwrap();
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => {
                assert!(
                    why.contains("config"),
                    "must say what is missing, got: {why}"
                )
            },
            other => panic!("a lone PK is not a cache, got {other:?}"),
        }
    }

    #[test]
    fn a_vk_that_does_not_deserialize_is_cold() {
        // Reaching the VK loader takes more setup than it looks: `{}` does
        // not deserialise as `BaseCircuitParams`, so a naive fixture returns
        // `Cold` at `load_config`, and even with a valid config it would
        // stop at the missing manifest.
        //
        // What it needs: a config that parses, a VK that does not, a
        // manifest at the current revision, and digests that match. Then
        // every earlier gate passes and the loader is the first thing that
        // can fail.
        //
        // No real keyset is required. The PK is a dummy file — once the VK
        // is rejected the probe returns and `try_load_pk` is never reached —
        // so this test runs everywhere and needs no `#[ignore]`.
        //
        // Scope: this covers the `Err` path, NOT the panic path.
        // `VerifyingKey::read` validates a version byte first and returns a
        // plain `io::Error` when it is wrong, so `b"not a…"` never reaches
        // the `RawBytesUnchecked` element reader that unwraps. The
        // `catch_unwind` regression is
        // `a_header_valid_but_truncated_vk_is_caught_not_a_panic` below,
        // which needs a real key to build a stream that gets past the
        // header.
        let d = TempDir::new().unwrap();
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        std::fs::write(d.path().join("event_vk.bin"), b"not a verifying key").unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 4096]).unwrap();
        save_manifest(d.path(), "event", EVENT_CIRCUIT_REVISION).unwrap();

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => assert!(
                why.contains("deserialise"),
                "must name the load failure, not an earlier gate; got: {why}"
            ),
            other => panic!("an unreadable vk must be cold, got {other:?}"),
        }
    }

    #[test]
    fn a_damaged_manifest_is_cold_and_does_not_borrow_the_absent_message() {
        // The two manifest states an `Option`-returning `read_manifest`
        // could not tell apart. Both are `Cold` — the remedy for either is
        // to regenerate, and `Corrupt` would suppress the `params_dir`
        // writability probe that regeneration depends on — but they must not
        // share a sentence: "this cache predates the check" printed over a
        // damaged file sends an operator looking for an upgrade they already
        // did.
        //
        // Plain, not `#[ignore]`d, and that is itself an assertion: the
        // probe reads the manifest before it hands anything to a halo2
        // reader, so dummy key bytes are enough to reach this gate. If a
        // later edit moves a deserialisation above it, this test starts
        // panicking inside the reader instead of failing an assert.
        let d = TempDir::new().unwrap();
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        std::fs::write(d.path().join("event_vk.bin"), vec![0u8; 64]).unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 4096]).unwrap();

        // 1. Absent: a cache written before this mechanism existed.
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => assert!(
                why.contains("no revision manifest"),
                "an absent manifest must be reported as absent; got: {why}"
            ),
            other => panic!("a trio with no manifest is cold, got {other:?}"),
        }

        // 2. Present and unparseable — truncated mid-string, which is what a non-atomic
        //    writer or an interrupted editor leaves.
        std::fs::write(
            d.path().join("event_manifest.json"),
            br#"{"circuit_revision": 1, "pk_sha256": "aa"#,
        )
        .unwrap();
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => {
                assert!(
                    why.contains("could not be read"),
                    "an unparseable manifest must name itself as unreadable; got: {why}"
                );
                assert!(
                    !why.contains("no revision manifest"),
                    "the unparseable manifest must not reuse the absent-manifest text; got: {why}"
                );
                // `{e:#}` in the probe, not `{e}`: plain `anyhow` Display
                // prints the outer "parse …" context and drops the serde
                // message under it, which is the half that says what is
                // wrong with the file.
                assert!(
                    why.contains("event_manifest.json"),
                    "the message must name the file; got: {why}"
                );
            },
            other => panic!("an unparseable manifest is cold, not {other:?}"),
        }
    }

    /// A `params_dir` with dummy bytes at all three key paths: enough to
    /// reach every gate past the first, so a test about the first gate fails
    /// for its own reason rather than for a missing file.
    fn dir_with_placeholder_keys() -> TempDir {
        let d = TempDir::new().unwrap();
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        std::fs::write(d.path().join("event_vk.bin"), vec![0u8; 64]).unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 4096]).unwrap();
        d
    }

    #[test]
    fn a_directory_at_a_key_path_is_blocked_not_cold() {
        // `Cold` is a promise that the run regenerates unattended.
        // Regeneration removes the manifest and renames four files into
        // place, and neither works through a directory — so `Cold` here is a
        // promise redeemed in stage 5, after the burn.
        //
        // Not a curiosity: `docker run -v
        // $HOST/event_pk.bin:/params/event_pk.bin` creates the mount point
        // as a directory when the host path does not exist, which is one
        // typo away from a normal deployment.
        for name in [
            "event_manifest.json",
            "event_pk.bin",
            "event_vk.bin",
            "event_config_params.json",
        ] {
            let d = dir_with_placeholder_keys();
            std::fs::remove_file(d.path().join(name)).ok();
            std::fs::create_dir(d.path().join(name)).unwrap();

            match probe_event_key_cache(d.path()) {
                KeyCacheState::Blocked {
                    why,
                } => {
                    assert!(why.contains(name), "must name the path; got: {why}");
                    assert!(
                        why.contains("is a directory"),
                        "must say what is wrong with it, not merely that something is; got: {why}"
                    );
                },
                other => panic!(
                    "a directory at {name} blocks regeneration, so it cannot be reported as \
                     {other:?}"
                ),
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn entries_that_rename_can_replace_are_not_blocked() {
        // The other half of the gate, and the half that is easy to get
        // wrong: `metadata` follows the final symlink, and `!is_file()` is
        // true of far more than a directory. Both errors refuse runs that
        // would have worked.
        //
        // Neither `unlink` nor `rename` follows the final path component, so
        // a symlink is replaced as a symlink no matter what it points at;
        // and a FIFO, socket or device node is an ordinary directory entry
        // that both operations handle. Only a real directory blocks them.
        //
        // `#[cfg(unix)]` because the whole gate is about Unix rename and
        // unlink semantics; there is nothing to assert elsewhere.
        use std::os::unix::fs::symlink;

        // 1. A symlink pointing at a directory. `metadata` reports "dir" here and
        //    `symlink_metadata` reports "symlink" — this is the exact case that
        //    separates them.
        let d = dir_with_placeholder_keys();
        let target = d.path().join("somewhere");
        std::fs::create_dir(&target).unwrap();
        let link = d.path().join("event_manifest.json");
        symlink(&target, &link).unwrap();
        assert!(
            !matches!(
                probe_event_key_cache(d.path()),
                KeyCacheState::Blocked { .. }
            ),
            "a symlink is replaced by rename as a symlink, whatever it points at"
        );

        // 2. A socket, which is the special file std can make without a new dependency
        //    (`mkfifo` would need libc). `unlink` removes it and `rename` replaces it,
        //    so the gate must let it through to be judged on its contents like any
        //    other file.
        let d = dir_with_placeholder_keys();
        std::fs::remove_file(d.path().join("event_pk.bin")).unwrap();
        let _sock = std::os::unix::net::UnixListener::bind(d.path().join("event_pk.bin")).unwrap();
        assert!(
            !matches!(
                probe_event_key_cache(d.path()),
                KeyCacheState::Blocked { .. }
            ),
            "a socket is an ordinary directory entry; unlink and rename both replace it"
        );
    }

    #[test]
    fn a_manifest_this_build_does_not_understand_is_cold() {
        // Two ways a future format can arrive, two mechanisms, one verdict.
        // `Cold` for both: a manifest written by another build is a
        // self-consistent cache belonging to that build, and this run fixes
        // it for itself by regenerating.
        let d = TempDir::new().unwrap();
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        std::fs::write(d.path().join("event_vk.bin"), vec![0u8; 64]).unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 4096]).unwrap();
        let path = d.path().join("event_manifest.json");
        let zero = "0".repeat(64);

        // 1. A field this build does not know. Serde's DEFAULT is to ignore it, which
        //    would let the probe walk on into the digest comparison as though the
        //    unknown field were not qualifying the fields it does know.
        //    `deny_unknown_fields` is what makes this a parse failure.
        std::fs::write(
            &path,
            serde_json::json!({
                "manifest_format": MANIFEST_FORMAT,
                "circuit_revision": EVENT_CIRCUIT_REVISION,
                "pk_sha256": &zero,
                "vk_sha256": &zero,
                "config_sha256": &zero,
                "srs_k": 20,
            })
            .to_string(),
        )
        .unwrap();
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => assert!(
                why.contains("could not be read"),
                "an unknown field must fail the parse rather than be ignored; got: {why}"
            ),
            other => panic!("expected Cold, got {other:?}"),
        }

        // 2. The same fields, a format this build does not write. This one parses
        //    cleanly — `deny_unknown_fields` has nothing to object to — so only the
        //    explicit version check catches it.
        //
        //    The digests here are deliberately wrong. Without the format
        //    check the probe would reach the digest comparison and answer
        //    `Corrupt`, so this assertion pins the ORDER as much as the
        //    verdict.
        std::fs::write(
            &path,
            serde_json::json!({
                "manifest_format": MANIFEST_FORMAT + 1,
                "circuit_revision": EVENT_CIRCUIT_REVISION,
                "pk_sha256": &zero,
                "vk_sha256": &zero,
                "config_sha256": &zero,
            })
            .to_string(),
        )
        .unwrap();
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => assert!(
                why.contains("manifest format"),
                "must name the format mismatch, not a later gate; got: {why}"
            ),
            other => panic!("a format this build cannot act on is cold, not {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_truncated_pk_beside_a_good_vk_is_corrupt_not_warm() {
        // The other false positive, and the expensive one: `keys_cached()`
        // returns true, keygen is skipped, and `load_pk` fails in stage 5 —
        // after the burn.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        // Truncate the PK and leave the manifest describing the full length
        // — which is exactly what an interrupted keygen produces if the
        // manifest was already there from a previous run, and what a
        // half-copied cache produces always.
        let pk = d.path().join("event_pk.bin");
        let full = std::fs::metadata(&pk).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&pk)
            .unwrap()
            .set_len(full / 2)
            .unwrap();
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Corrupt {
                why,
            } => {
                assert!(why.contains("truncated"), "got: {why}")
            },
            other => panic!("a half-length PK must not read as usable, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_complete_cache_is_warm() {
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Warm
        ));
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS + _ALT; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_grafted_vk_is_corrupt_not_warm() {
        // A's config and PK with B's verifying key on top: each file loads,
        // the pair does not match. The proof is produced against the PK's
        // embedded VK and self-verified against `event_vk.bin` — which
        // fails, in stage 5, after the burn.
        let Some(a) = fixture_with_real_event_keys() else {
            return;
        };
        let Some(b) = fixture_with_alternate_event_keys() else {
            return;
        };
        std::fs::copy(b.path().join("event_vk.bin"), a.path().join("event_vk.bin")).unwrap();

        match probe_event_key_cache(a.path()) {
            KeyCacheState::Corrupt {
                why,
            } => {
                assert!(why.contains("verifying key"), "got: {why}")
            },
            other => panic!("a mismatched pk/vk pair must not read as warm, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_same_length_byte_flip_in_the_pk_is_corrupt_not_warm() {
        // The case a length check cannot see and a `transcript_repr()`
        // comparison cannot see either: `RawBytesUnchecked` skips the
        // canonical/on-curve checks, so a flipped coefficient past the
        // embedded VK deserialises fine, the file keeps its size, and the VK
        // digest is unchanged. Only a digest over the PK's own bytes catches
        // it.
        use std::io::{Read, Seek, SeekFrom, Write};

        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        let pk = d.path().join("event_pk.bin");
        let len = std::fs::metadata(&pk).unwrap().len();
        {
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&pk)
                .unwrap();
            // Well past the embedded verifying key, so the failure is in the
            // proving key proper.
            //
            // Read then XOR, rather than writing a constant: `0xFF` over a
            // byte that is already `0xFF` changes nothing, the digest
            // matches, and the test passes or fails depending on the
            // contents of a key nobody controls.
            let mut byte = [0u8; 1];
            f.seek(SeekFrom::Start(len - 1024)).unwrap();
            f.read_exact(&mut byte).unwrap();
            f.seek(SeekFrom::Start(len - 1024)).unwrap();
            f.write_all(&[byte[0] ^ 0x01]).unwrap();
            f.sync_all().unwrap();
        }
        assert_eq!(
            std::fs::metadata(&pk).unwrap().len(),
            len,
            "length must be unchanged"
        );

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Corrupt {
                why,
            } => {
                assert!(why.contains("proving key"), "got: {why}")
            },
            other => panic!("a corrupted proving key must not read as warm, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_header_valid_but_truncated_vk_is_caught_not_a_panic() {
        // THE `catch_unwind` regression. Nothing else reaches the panic.
        //
        // Two ways to miss it:
        //
        //  - garbage from byte zero (`a_vk_that_does_not_deserialize_is_cold`) fails
        //    the version-byte check and returns a clean `Err`;
        //  - a truncated VK with the original manifest
        //    (`a_truncated_vk_is_corrupt_not_warm` below) stops at the digest
        //    comparison, one gate earlier.
        //
        // A real VK truncated mid-body, with the digest recomputed, is the
        // only fixture that gets past the header AND past the digests, so
        // that `read_exact` runs out inside an element and unwraps.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        let vk = d.path().join("event_vk.bin");
        let len = std::fs::metadata(&vk).unwrap().len();
        assert!(len > 64, "a real vk is longer than its header");
        {
            let f = std::fs::OpenOptions::new().write(true).open(&vk).unwrap();
            // Past the version byte, k, and compress_selectors; nowhere near
            // the end.
            f.set_len(len / 2).unwrap();
            f.sync_all().unwrap();
        }
        rewrite_manifest_digests(d.path());

        // Without `catch_unwind` in `try_load_vk`, this line aborts the test
        // process rather than failing an assertion.
        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => {
                assert!(why.contains("deserialise"), "got: {why}")
            },
            other => panic!("a truncated vk must be reported, not panicked on, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_truncated_vk_is_corrupt_not_warm() {
        // A truncated VK whose manifest still records the original digest:
        // the digest comparison catches it, one gate before the loader. That
        // ordering is the point — nothing malformed reaches a reader while a
        // cheaper check can reject it. The loader's own safety is covered by
        // the test above.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        let vk = d.path().join("event_vk.bin");
        let len = std::fs::metadata(&vk).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&vk)
            .unwrap()
            .set_len(len / 2)
            .unwrap();

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Corrupt {
                why,
            } => {
                assert!(why.contains("verifying key"), "got: {why}")
            },
            other => panic!("a truncated vk must be reported, not panicked on, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS + _ALT; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_complete_but_stale_trio_is_cold_not_warm() {
        // The case the pk/vk comparison alone cannot see, and the one a
        // circuit change actually produces: every file comes from the same
        // older keygen, so config, pk and vk agree with each other
        // perfectly. Only the manifest separates them from this build.
        let Some(d) = fixture_with_alternate_event_keys() else {
            return;
        };
        // Write the manifest here rather than expecting the fixture to carry
        // one: the alternate keyset comes from a tag that predates
        // manifests, so it has none. This is also what makes the test
        // precise — the ONLY thing separating this trio from a warm cache is
        // the revision number it declares.
        save_manifest(d.path(), "event", EVENT_CIRCUIT_REVISION - 1).unwrap();

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => {
                assert!(
                    why.contains("revision"),
                    "must name the mismatch, got: {why}"
                )
            },
            other => panic!("a stale trio must be regenerated, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn an_unloadable_pk_with_a_matching_digest_is_corrupt() {
        // The branch that exists because `keys_cached()` gets it wrong, and
        // the one no other test reaches: every other corrupt-PK case stops
        // at the digest comparison. Re-record the digest over the damaged
        // bytes so the manifest agrees with the file, and the probe is
        // forced all the way to `try_load_pk`.
        //
        // This is not a contrived state. It is what a key from an older
        // proving-key layout looks like after a halo2 bump: internally
        // consistent, correctly recorded, and unreadable by this build.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        let pk = d.path().join("event_pk.bin");

        // TRUNCATE, do not scribble.
        //
        // Overwriting bytes in place and keeping the length does not make
        // the file unloadable: `RawBytesUnchecked` performs no canonical or
        // on-curve checks, so altered coefficients deserialise happily —
        // which is exactly why the same-length byte flip needed a digest to
        // catch it, a few tests up. That fixture would leave this test
        // asserting `Corrupt` against a probe that returns `Warm`.
        //
        // Truncation is the one damage the reader cannot absorb:
        // `read_exact` runs out of input and panics, and the panic-safe
        // wrapper turns that into `None`. Cut after the embedded verifying
        // key so the VK still loads and only the PK arm is exercised.
        {
            let f = std::fs::OpenOptions::new().write(true).open(&pk).unwrap();
            let len = f.metadata().unwrap().len();
            // Two thirds: comfortably past the VK, comfortably short of the
            // polynomials the reader still expects.
            f.set_len(len * 2 / 3).unwrap();
            f.sync_all().unwrap();
        }
        // Re-record the digests over the truncated file, so the digest check
        // passes and the probe is forced on to `try_load_pk`. Without this
        // the test would stop one branch early, which is the gap it exists
        // to close.
        rewrite_manifest_digests(d.path());

        // The runtime predicate is fooled — that is the whole point.
        let km = EventKeyManager::new(d.path());
        assert!(
            km.keys_cached(),
            "keys_cached only checks that the PK file exists; if this ever starts returning false \
             the Corrupt classification below can be relaxed"
        );

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Corrupt {
                why,
            } => {
                assert!(why.contains("proving key"), "got: {why}")
            },
            other => panic!(
                "a proving key this build cannot read must be refused, not reported as a keygen \
                 the runtime will skip; got {other:?}"
            ),
        }
    }

    /// Recompute every digest in the manifest from the files as they are
    /// now, leaving `circuit_revision` alone.
    ///
    /// Only useful for tests that need to get *past* the digest check.
    fn rewrite_manifest_digests(dir: &Path) {
        let path = dir.join("event_manifest.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (field, file) in [
            ("pk_sha256", "event_pk.bin"),
            ("vk_sha256", "event_vk.bin"),
            ("config_sha256", "event_config_params.json"),
        ] {
            m[field] = serde_json::json!(sha256_file(&dir.join(file)).unwrap());
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_malformed_digest_is_corrupt_not_cold() {
        // `Cold` is a promise the runtime does not keep here. The manifest
        // is valid JSON at the current revision, so `new` installs the VK;
        // `keys_cached()` then sees a VK and a PK file and answers true,
        // keygen is skipped, and the run dies after the burn. (`keys_cached()`
        // does not itself check the revision or the digests — `new` does,
        // and it is satisfied: the vk and config digests are untouched here,
        // only the pk digest is malformed.) The distinction between Cold and
        // Corrupt IS the regression.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };

        // Current revision — so the revision check passes — with a digest
        // that is not 64 hex characters. Three shapes, one per way the guard
        // can trip:
        //
        //  - too short;
        //  - 64 bytes, ASCII, not hex — this is the one that actually reaches
        //    `is_ascii_hexdigit`;
        //  - 64 *characters* of non-ASCII, which is 128 bytes and so trips the length
        //    check. Worth keeping precisely because it looks like a hex-check case and
        //    is not.
        for bad in ["deadbeef", &"g".repeat(64), &"ю".repeat(64)] {
            let path = d.path().join("event_manifest.json");
            let mut m: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            m["pk_sha256"] = serde_json::json!(bad);
            std::fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

            match probe_event_key_cache(d.path()) {
                KeyCacheState::Corrupt {
                    why,
                } => {
                    assert!(why.contains("digest"), "got: {why}")
                },
                other => panic!(
                    "a digest that cannot match any file must refuse, not promise a keygen the \
                     runtime will skip; got {other:?}"
                ),
            }
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_trio_with_no_manifest_is_cold() {
        // Caches written before this mechanism existed. Regenerating costs
        // one keygen; trusting them costs a burn.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        std::fs::remove_file(d.path().join("event_manifest.json")).ok();
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Cold { .. }
        ));
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn an_interrupted_regeneration_reads_as_cold_not_corrupt() {
        // The case the retraction exists for, stopped where a crash would
        // stop it: manifest retracted, keys partly rewritten.
        //
        // Before the retraction this state was "old manifest beside new
        // keys" and reported `Corrupt` — a refusal demanding a manual
        // --repair for what is simply an unfinished keygen. After it, the
        // window has no manifest at all, which is `Cold`, and regenerates
        // unattended.
        //
        // The retraction is performed by PRODUCTION code, not by the test.
        // Calling `remove_file` here directly would assert on a state the
        // test itself created and pass even if `run_keygen` never retracted
        // anything.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };

        let km = EventKeyManager::new(d.path());
        km.retract_manifest()
            .expect("retraction must succeed on a writable dir");
        assert!(
            !d.path().join("event_manifest.json").exists(),
            "the helper removes the manifest"
        );

        // ...and then a partial rewrite, interrupted.
        std::fs::write(d.path().join("event_vk.bin"), b"half-written").unwrap();

        match probe_event_key_cache(d.path()) {
            KeyCacheState::Cold {
                why,
            } => assert!(
                why.contains("manifest"),
                "an interrupted keygen must read as cold, not demand a repair; got: {why}"
            ),
            other => panic!("expected Cold for an interrupted regeneration, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn run_keygen_republishes_a_consistent_manifest() {
        // The test above proves the helper works. This one proves
        // `run_keygen` is wired to it end to end — a helper nobody calls is
        // exactly the shape to guard against.
        //
        // A cold cache is required, or `ensure_keys` returns before
        // `run_keygen` is reached. Remove the proving key: config and vk
        // stay, so this is `Cold` for the reason that forces a keygen rather
        // than a reason that would also reject the fixture.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        std::fs::remove_file(d.path().join("event_pk.bin")).unwrap();
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Cold { .. }
        ));

        let mut km = EventKeyManager::new(d.path());
        km.ensure_keys()
            .expect("keygen must succeed on a writable params dir");

        // Not `let _ =`: a silently-failed keygen would leave the old
        // manifest in place and make the next assertion pass for the wrong
        // reason.
        assert!(
            matches!(probe_event_key_cache(d.path()), KeyCacheState::Warm),
            "after a keygen the manifest must describe the files beside it"
        );
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_stale_trio_is_refused_at_load_by_every_consumer() {
        // Three things must agree on a stale cache:
        //
        //   * `probe_event_key_cache` -> Cold        (preflight)
        //   * `keys_cached()`         -> false       (ensure_keys)
        //   * `vk_opt()`              -> None        (everyone else)
        //
        // The last matters because `bridge-verifier-daemon` reads `vk_opt()`
        // and nothing else. Enforcing the revision only in `keys_cached()`
        // would leave that consumer verifying against another revision's
        // key.
        let Some(d) = fixture_with_real_event_keys() else {
            return;
        };
        save_manifest(d.path(), "event", EVENT_CIRCUIT_REVISION - 1).unwrap();

        let km = EventKeyManager::new(d.path());
        assert!(
            km.vk_opt().is_none(),
            "a stale verifying key must not be installed — this is what protects consumers that \
             never call ensure_keys"
        );
        assert!(!km.keys_cached(), "a stale trio is not a cache hit");
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Cold { .. }
        ));

        // The same three, for a manifest whose FORMAT this build does not
        // write. Folded in here rather than given its own test because the
        // property is identical and the fixture is the expensive part — this
        // copies a ~2.65 GB proving key.
        //
        // It is not covered by the probe test that exercises
        // `manifest_format`: that one only calls `probe_event_key_cache`,
        // and the gate protecting `bridge-verifier-daemon` is a separate
        // branch inside `KeyManagerState::new`.
        //
        // Written by hand: `save_manifest` always stamps the current format,
        // which is the right behaviour and the reason it cannot produce this
        // fixture.
        let path = d.path().join("event_manifest.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        m["circuit_revision"] = serde_json::json!(EVENT_CIRCUIT_REVISION);
        m["manifest_format"] = serde_json::json!(MANIFEST_FORMAT + 1);
        std::fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        // Digests recomputed, so nothing but the format is out of place and
        // a pass cannot come from an unrelated mismatch.
        rewrite_manifest_digests(d.path());
        m = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            m["manifest_format"],
            serde_json::json!(MANIFEST_FORMAT + 1),
            "rewrite_manifest_digests must not have reset the format"
        );

        let km = EventKeyManager::new(d.path());
        assert!(
            km.vk_opt().is_none(),
            "a manifest in an unknown format must not qualify a verifying key for the daemon \
             either"
        );
        assert!(!km.keys_cached(), "an unreadable record is not a cache hit");
        assert!(matches!(
            probe_event_key_cache(d.path()),
            KeyCacheState::Cold { .. }
        ));
    }

    #[test]
    #[ignore = "needs BRIDGE_TEST_EVENT_KEYS + _ALT; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_grafted_vk_is_not_installed_either() {
        // Split out rather than appended to the test above. Folding it in
        // would put a second `else { return }` in the middle of that
        // function, so a host without the ALT keyset would skip the
        // primary-only assertions that followed it — the ones that need no
        // ALT at all.
        //
        // This is the case the revision check alone cannot see: current
        // revision, a valid verifying key, the wrong keyset.
        let Some(g) = fixture_with_real_event_keys() else {
            return;
        };
        let Some(alt) = fixture_with_alternate_event_keys() else {
            return;
        };
        std::fs::copy(
            alt.path().join("event_vk.bin"),
            g.path().join("event_vk.bin"),
        )
        .unwrap();

        assert!(
            EventKeyManager::new(g.path()).vk_opt().is_none(),
            "a grafted verifying key must not be installed — the daemon reads vk_opt() and \
             nothing else"
        );
    }

    /// A `params_dir` holding genuine event keys, or `None` when this host
    /// has none cached.
    ///
    /// `Option`, not `TempDir`: these keys cannot be synthesised —
    /// `try_load_pk` deserialises against the real circuit — so a host
    /// without them has nothing to hand back, and the skip has to live in
    /// the type rather than in a `return` the compiler will reject.
    ///
    /// Copies from `BRIDGE_TEST_EVENT_KEYS` rather than generating: keygen
    /// is ~7 min and ~3 GB, which does not belong inside `cargo test`.
    fn fixture_with_real_event_keys() -> Option<TempDir> {
        copy_keyset_from("BRIDGE_TEST_EVENT_KEYS")
    }

    /// A second keyset that loads cleanly but does not match the first.
    ///
    /// "Does not match" means a different circuit revision under the same
    /// `BaseCircuitParams`, so config and both files still deserialise and
    /// only the manifest separates them. That is the case `keys_cached()`
    /// cannot see.
    fn fixture_with_alternate_event_keys() -> Option<TempDir> {
        copy_keyset_from("BRIDGE_TEST_EVENT_KEYS_ALT")
    }

    /// Copy the three event-key files named by `var` into a fresh `TempDir`,
    /// so a test may truncate or overwrite them without destroying the cache
    /// it came from.
    fn copy_keyset_from(var: &str) -> Option<TempDir> {
        // The ceremony comes too. `KeyManagerState::new` calls
        // `load_srs(params_dir, 20)` before anything else, and `load_srs`
        // panics when the directory holds no Hermez file at all — so any
        // test that constructs an `EventKeyManager` against a fixture dies
        // there, long before its assertion. `probe_event_key_cache` itself
        // does not need the SRS; the `keys_cached`/probe agreement test
        // does.
        //
        // Copying is cheap relative to the 2.65 GB PK already being copied.
        const CEREMONY: &str = "kzg_bn254_20.srs";

        // The three real key files are required; the manifest is NOT.
        //
        // This distinction is load-bearing. The alternate keyset comes from
        // a tag that predates the manifest, so it has only the three — and
        // requiring four here would make `copy_keyset_from` return `None`
        // for it, which turns all three alternate-keyset tests into
        // permanent green skips. A missing manifest is also a real state in
        // the field (every cache written before this change), and one of the
        // tests exists specifically to exercise it.
        const REQUIRED: [&str; 3] = ["event_config_params.json", "event_vk.bin", "event_pk.bin"];
        const OPTIONAL: [&str; 1] = ["event_manifest.json"];

        // `None` means exactly one thing: nobody asked this host to run
        // these tests. Everything past that point is a failure, because a
        // variable that IS set is a statement that the keyset exists — and
        // degrading a broken fixture into a skip is how tests end up
        // reporting green while checking nothing.
        let Some(src) = std::env::var_os(var).map(std::path::PathBuf::from) else {
            eprintln!("skipping: {var} is unset (see the runbook on populating it)");
            return None;
        };
        for f in REQUIRED {
            assert!(
                src.join(f).is_file(),
                "{var}={} is set but {f} is missing — an incomplete keyset must fail, not \
                 silently skip the tests that depend on it",
                src.display()
            );
        }
        // From here on, failures are failures. `None` means "this host was
        // not asked to run these tests"; a copy that dies halfway means
        // something is wrong, and `.ok()?` would report it as the former.
        let dst = TempDir::new().expect("temp dir for the key fixture");
        for f in REQUIRED {
            std::fs::copy(src.join(f), dst.path().join(f))
                .unwrap_or_else(|e| panic!("copy {f} from {}: {e}", src.display()));
        }
        for f in OPTIONAL {
            if src.join(f).is_file() {
                std::fs::copy(src.join(f), dst.path().join(f))
                    .unwrap_or_else(|e| panic!("copy {f} from {}: {e}", src.display()));
            }
        }
        // Prefer the exact degree; fall back to any larger ceremony and let
        // `load_srs` downsize, exactly as a real run would.
        let ceremony = [CEREMONY, "kzg_bn254_21.srs"]
            .into_iter()
            .find(|f| src.join(f).is_file());
        match ceremony {
            Some(f) => {
                std::fs::copy(src.join(f), dst.path().join(f))
                    .unwrap_or_else(|e| panic!("copy {f} from {}: {e}", src.display()));
            },
            None => panic!(
                "{var}={} is set but holds no kzg_bn254_20/21.srs — `KeyManagerState::new` loads \
                 the SRS before anything else, so the fixture cannot be built without one",
                src.display()
            ),
        }
        Some(dst)
    }
}
