//! Persistent PK cache for the outer SHPLONK aggregator keygen.
//!
//! `aggregate_and_prove` / `aggregate_inner` / `generate_yul_verifier` all
//! previously called `gen_pk(agg_params, &circuit, None)` on every
//! invocation, forcing a full outer keygen at K=21 — most of a cold
//! withdrawal proof's ~20 min. Every rerun of `export-inner-aggregator` and
//! every runtime call to `aggregate-proof` paid that cost from scratch.
//!
//! This module memoises the outer
//! `(pk, break_points, calculated, num_instance)` bundle on disk, keyed by a
//! **content hash** over the inputs that actually determine the outer PK:
//! `AggregatorConfig`, the SRS `s_g2` head, and `bincode(inner_snark.protocol)`
//! (a stable function of the inner VK).
//!
//! Cache slot layout under `<cache_dir>`:
//! - `<stem>.pk`         -- proving key (SDK's `RawBytes` format, ~6.8 GB for
//!   the withdrawal aggregator @ K=21)
//! - `<stem>.meta.json`  -- break_points + calculated params + num_instance
//!
//! Both are written under a `.partial-<writer>` name, synced, and renamed
//! into place when complete, the key first: a slot whose meta exists has a
//! whole key, after a crash of the host too.
//! A writer holds `<cache_dir>/.keygen.lock` meanwhile, which is how a
//! leftover partial file is told from one being written.
//!
//! Slot stem: `<base_name>__v3__<content_hash:hex[..32]>`. `base_name` is
//! a human-readable label only — it is **not** trusted for correctness. Two
//! callers using the same `base_name` with distinct inner VKs, distinct
//! aggregator configs, or a distinct SRS produce distinct content hashes, so
//! they can never share a slot. Conversely, two callers producing identical
//! content share a slot even under different names (deemed acceptable — this
//! is the classical cache-dedup property).
//!
//! Format tag `v3` in the stem gates against silent breakage if the hash
//! preimage layout ever changes; bump to `v4` to invalidate all v3 slots.
//!
//! History:
//!
//! - v1 used a 64-bit `SipHash` of the protocol bytes alone plus trusted
//!   `base_name`; the SRS was **not** in the key. That meant a
//!   re-bootstrapped SRS or an unlucky hash collision under the same
//!   `base_name` could silently serve stale keys. The `bin/aggregate_proof.rs`
//!   byte-drift check would catch it before on-chain use, but the cache-side
//!   precondition is now enforced properly.
//! - v1→v2 widened the content hash to include the SRS `s_g2` head so a
//!   re-bootstrapped SRS never shares a slot with the old one.
//! - v2→v3 was bumped when `expose_vk_digest` was added inside the keygen
//!   circuit. The hash preimage layout itself did not change, but the outer
//!   PK, `calculated` params, and `num_instance` all did — so pre-v3 cache
//!   slots produce a Yul verifier that no longer matches the on-chain
//!   adapters and must not be reused. The content hash cannot notice this
//!   because it is computed from the inner-snark protocol, not the outer
//!   circuit synthesis; the stem tag is the guard.

use std::{
    fs::File,
    io::{BufReader, BufWriter, Write as _},
    path::{Path, PathBuf},
};

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::{bn256::{Bn256, G1Affine}, serde::SerdeObject},
        plonk::{Circuit as _, ProvingKey},
        poly::kzg::commitment::ParamsKZG,
        SerdeFormat,
    },
};
use anyhow::Context;
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use snark_verifier_sdk::{
    gen_pk,
    halo2::aggregation::{
        AggregationCircuit, AggregationConfigParams, VerifierUniversality,
    },
    CircuitExt, Snark, SHPLONK,
};

use crate::aggregator::AggregatorConfig;

/// Outer-aggregator keygen artefacts, either freshly generated or reloaded
/// from disk. The caller uses these to build the prover-stage circuit
/// (`calculated` + `break_points`) and to invoke `gen_evm_verifier_shplonk`
/// (`pk.get_vk()` + `num_instance`).
pub struct CachedKeygen {
    pub pk: ProvingKey<G1Affine>,
    pub break_points: Vec<Vec<usize>>,
    pub calculated: AggregationConfigParams,
    pub num_instance: Vec<usize>,
}

/// Slot paths (`.pk` + `.meta.json`) inside `cache_dir`.
struct SlotPaths {
    pk: PathBuf,
    meta: PathBuf,
}

/// On-disk mirror of everything the SDK's `gen_pk(Some(path))` does *not*
/// persist. Uses local mirror structs instead of relying on
/// `AggregationConfigParams: Serialize` -- serde derive status is
/// version-dependent, and mirroring costs 20 lines.
#[derive(Serialize, Deserialize)]
struct CachedMeta {
    break_points: Vec<Vec<usize>>,
    calculated: MirrorConfigParams,
    num_instance: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct MirrorConfigParams {
    degree: u32,
    num_advice: usize,
    num_lookup_advice: usize,
    num_fixed: usize,
    lookup_bits: usize,
}

impl From<AggregationConfigParams> for MirrorConfigParams {
    fn from(p: AggregationConfigParams) -> Self {
        Self {
            degree: p.degree,
            num_advice: p.num_advice,
            num_lookup_advice: p.num_lookup_advice,
            num_fixed: p.num_fixed,
            lookup_bits: p.lookup_bits,
        }
    }
}

impl From<MirrorConfigParams> for AggregationConfigParams {
    fn from(p: MirrorConfigParams) -> Self {
        Self {
            degree: p.degree,
            num_advice: p.num_advice,
            num_lookup_advice: p.num_lookup_advice,
            num_fixed: p.num_fixed,
            lookup_bits: p.lookup_bits,
        }
    }
}

fn universality_byte(u: VerifierUniversality) -> u8 {
    match u {
        VerifierUniversality::None => 0,
        VerifierUniversality::PreprocessedAsWitness => 1,
        VerifierUniversality::Full => 2,
    }
}

/// SRS `s_g2` bytes — the public MPC-ceremony fingerprint. Fresh SRS ⇒ fresh
/// bytes ⇒ fresh cache slot. Same encoding path as `srs_guard::assert_hermez_ceremony`.
fn srs_s_g2_bytes(params: &ParamsKZG<Bn256>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(128);
    params
        .s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    buf
}

fn snark_protocol_bytes(inner_snark: &Snark) -> Vec<u8> {
    bincode::serialize(&inner_snark.protocol)
        .expect("PlonkProtocol serialization is infallible for well-formed snarks")
}

/// Content hash — SHA-256 over the domain-tagged, length-prefixed concatenation
/// of every input that determines the outer PK. Two calls collide iff the
/// caller passed identical config + SRS + inner-snark protocol bytes.
///
/// Preimage layout (all little-endian):
/// ```text
///   b"bridge-evm-aggregator-cache-v3"
///   u32(k_outer) || u32(lookup_bits_outer) || u8(universality)
///   u32(s_g2_len)     || s_g2_bytes
///   u32(protocol_len) || protocol_bytes
/// ```
fn content_hash(
    config: &AggregatorConfig,
    s_g2_bytes: &[u8],
    protocol_bytes: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"bridge-evm-aggregator-cache-v3");
    h.update((config.k_outer as u32).to_le_bytes());
    h.update((config.lookup_bits_outer as u32).to_le_bytes());
    h.update([universality_byte(config.universality)]);
    h.update((s_g2_bytes.len() as u32).to_le_bytes());
    h.update(s_g2_bytes);
    h.update((protocol_bytes.len() as u32).to_le_bytes());
    h.update(protocol_bytes);
    h.finalize().into()
}

/// Deterministic slot stem: `<base_name>__v3__<content_hash[..32]>`.
///
/// `base_name` is a human-readable label only. Correctness is enforced by the
/// content hash — see module-level docs.
pub fn cache_stem(
    base_name: &str,
    config: &AggregatorConfig,
    srs: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
) -> String {
    let hash = content_hash(
        config,
        &srs_s_g2_bytes(srs),
        &snark_protocol_bytes(inner_snark),
    );
    // 128 bits of the 256-bit digest keeps filenames short; a full collision
    // there is still infeasible and the byte-drift check in aggregate_proof
    // provides defence in depth.
    format!("{base_name}__v3__{}", hex::encode(&hash[..16]))
}

fn slot_paths(cache_dir: &Path, stem: &str) -> SlotPaths {
    SlotPaths {
        pk: cache_dir.join(format!("{stem}.pk")),
        meta: cache_dir.join(format!("{stem}.meta.json")),
    }
}

/// Suffix of a slot file still being written: `<stem>.pk.partial-<writer>`
/// and `<stem>.meta.json.partial-<writer>`, renamed into place only once
/// complete and synced. A writer killed mid-way — a caller's timeout, the OOM
/// killer — leaves one of these and never a truncated `<stem>.pk`: the SDK's
/// `read_pk` panics on a truncated key instead of regenerating it, so a
/// truncated slot under the real name failed every later run until someone
/// deleted it by hand. A leftover is removed by the next process that holds
/// [`KEYGEN_LOCK`]; `ackinacki-bridge`'s preflight looks for this suffix to
/// name it.
pub const PARTIAL_SUFFIX: &str = ".partial-";

/// Where `writer` writes `final_path` before publishing it.
fn partial_path(final_path: &Path, writer: &str) -> PathBuf {
    let mut name = final_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!("{PARTIAL_SUFFIX}{writer}"));
    final_path.with_file_name(name)
}

/// This process's name for its partial files: the pid, for whoever reads
/// the directory, and a random part, because a pid is unique only inside
/// its own namespace and a host and a container can share a cache.
fn writer_tag() -> String {
    format!("{}-{:08x}", std::process::id(), rand::random::<u32>())
}

/// `<cache_dir>/.keygen.lock`: an exclusive `flock(2)` a process holds from
/// the moment it decides to generate a slot until both of the slot's files
/// are published. While nobody holds it, nobody is writing the cache, so
/// every partial file in it is a leftover; whoever holds it may remove them.
///
/// A lock and not a pid, because the cache is shared across PID namespaces
/// — a host CLI and a relayer container on one bind-mounted `pk_cache` — and
/// a live writer's pid looks absent from the other side. `flock` belongs to
/// the open file, the kernel drops it when its holder dies, and it means the
/// same thing to every process on the host.
const KEYGEN_LOCK: &str = ".keygen.lock";

/// The cache's keygen lock file, opened (and created) without truncating.
/// A file another user created and this one may not write is opened
/// read-only: `flock` needs only an open file, and running unlocked beside
/// a user who can lock is what this lock exists to prevent.
fn open_keygen_lock(dir: &Path) -> anyhow::Result<std::fs::File> {
    let path = dir.join(KEYGEN_LOCK);
    let read_write = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path);
    match read_write {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => std::fs::File::open(&path)
            .with_context(|| format!("open cache lock {}", path.display())),
        Err(e) => Err(e).with_context(|| format!("open cache lock {}", path.display())),
    }
}

/// Linux errno values meaning "this filesystem cannot lock at all" —
/// `ENOLCK`, `ENOSYS`, `EOPNOTSUPP` (the release binary is Linux-only).
const LOCKING_UNSUPPORTED: [i32; 3] = [37, 38, 95];

/// Whether `e` says the filesystem cannot lock, the one failure a keygen may
/// run unlocked through. Anything else — a permission, an I/O error — is an
/// error: the process next door may well be able to lock, and would sweep
/// this one's partial file.
fn locking_unsupported(e: &std::io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|n| LOCKING_UNSUPPORTED.contains(&n))
}

/// The keygen lock if nobody holds it; `None` while a writer is at work.
fn try_lock_cache(dir: &Path) -> anyhow::Result<Option<std::fs::File>> {
    let file = open_keygen_lock(dir)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("lock {}", dir.join(KEYGEN_LOCK).display()))
        },
    }
}

/// Take the keygen lock, waiting while another process generates a key in
/// this cache. `Ok(None)` only where the filesystem cannot lock: the keygen
/// then runs unlocked, and nothing is swept. Every other failure is an
/// error.
fn lock_cache(dir: &Path) -> anyhow::Result<Option<std::fs::File>> {
    let file = open_keygen_lock(dir)?;
    let locked = match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => {
            tracing::info!(
                target: "bridge_evm_aggregator::cache",
                cache_dir = %dir.display(),
                "another aggregate-proof is generating a key in this cache; waiting for it"
            );
            file.lock()
        },
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    };
    match locked {
        Ok(()) => Ok(Some(file)),
        Err(e) if locking_unsupported(&e) => {
            tracing::warn!(
                target: "bridge_evm_aggregator::cache",
                error = %e,
                "this filesystem cannot lock the pk cache; generating the key without the \
                 lock and leaving partial files alone"
            );
            Ok(None)
        },
        Err(e) => Err(e).with_context(|| format!("lock {}", dir.join(KEYGEN_LOCK).display())),
    }
}

/// Remove every partial slot file in `dir` and return them. Only for a
/// holder of the keygen lock: then none of them has a live writer.
fn sweep_partials(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let partial = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.contains(PARTIAL_SUFFIX));
        if partial && std::fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

/// [`sweep_partials`], and say what went.
fn sweep_and_report(dir: &Path) {
    for leftover in sweep_partials(dir) {
        tracing::warn!(
            target: "bridge_evm_aggregator::cache",
            path = %leftover.display(),
            "removed a partial cache file an interrupted aggregate-proof left behind"
        );
    }
}

/// Remove a `.pk` that has no `.meta.json` beside it. It was never
/// published — a writer from before slots were published, killed mid-way,
/// or one killed between the two renames — and `gen_pk` must not read it.
/// Removed rather than left, so the keygen that follows has its room.
fn clear_unpublished_slot(s: &SlotPaths) -> std::io::Result<()> {
    if s.pk.exists() && !s.meta.exists() {
        std::fs::remove_file(&s.pk)?;
    }
    Ok(())
}

/// Create `path` — never over an existing file — fill it through
/// `write`, and sync it.
fn write_synced(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    fill_and_sync(file, write).with_context(|| format!("write {}", path.display()))
}

/// Fill `file` through a buffer and sync it. The last buffer is flushed
/// here, not on drop, which would throw its error away: a key whose tail
/// never reached the disk is not whole.
fn fill_and_sync(
    file: File,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut w = BufWriter::with_capacity(IO_BUFFER, file);
    write(&mut w)?;
    w.into_inner().map_err(|e| e.into_error())?.sync_all()
}

/// Sync a directory, so that a rename in it outlives a crash. A filesystem
/// that cannot sync a directory says `EINVAL`; there is nothing more to do
/// on it.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    match File::open(dir)?.sync_all() {
        Err(e) if e.raw_os_error() == Some(22) => Ok(()),
        other => other,
    }
}

/// Move a finished, synced `partial` onto `final_path` in one step, and
/// make the move durable.
fn publish(partial: &Path, final_path: &Path) -> anyhow::Result<()> {
    std::fs::rename(partial, final_path).with_context(|| {
        format!(
            "publish cache slot file {} as {}",
            partial.display(),
            final_path.display()
        )
    })?;
    let dir = final_path.parent().unwrap_or(Path::new("."));
    sync_dir(dir).with_context(|| format!("sync {}", dir.display()))
}

/// Buffer size for a key's reads and writes, the SDK's own.
const IO_BUFFER: usize = 1024 * 1024;

/// Read a published key. Not through `gen_pk(.., Some(path))`: that runs a
/// whole keygen when the file cannot be opened and writes the result over
/// the published name, without the lock, and it panics on a truncated key.
fn read_slot_key(
    path: &Path,
    params: AggregationConfigParams,
) -> anyhow::Result<ProvingKey<G1Affine>> {
    let unreadable = || {
        format!(
            "read cached aggregator key {}. Remove it and the .meta.json beside it to have both \
             generated again",
            path.display()
        )
    };
    let file = File::open(path).with_context(unreadable)?;
    let mut r = BufReader::with_capacity(IO_BUFFER, file);
    ProvingKey::read::<_, AggregationCircuit>(&mut r, SerdeFormat::RawBytesUnchecked, params)
        .with_context(unreadable)
}

/// Whether a slot is published: its meta is renamed into place after its
/// key, so a meta means a whole key.
fn published(s: &SlotPaths) -> bool {
    s.pk.exists() && s.meta.exists()
}

/// Read a published slot: the key, and the rest from the meta.
fn load_slot(
    agg_params: &ParamsKZG<Bn256>,
    s: &SlotPaths,
    inner_snark: &Snark,
    universality: VerifierUniversality,
) -> anyhow::Result<CachedKeygen> {
    let meta_text = std::fs::read_to_string(&s.meta)
        .with_context(|| format!("read cache meta {}", s.meta.display()))?;
    let meta: CachedMeta = serde_json::from_str(&meta_text)
        .with_context(|| format!("parse cache meta {}", s.meta.display()))?;
    let calculated: AggregationConfigParams = meta.calculated.into();

    // Build a "post-keygen shape" circuit for the params the key is read
    // with. No synthesis runs. We omit `expose_previous_instances` /
    // `calculate_params` deliberately: both `num_instance` and
    // `break_points` are recovered from `meta` so re-deriving them here
    // would be pure overhead.
    let circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        calculated,
        agg_params,
        vec![inner_snark.clone()],
        universality,
    );
    let pk = read_slot_key(&s.pk, circuit.params())?;
    tracing::info!(
        target: "bridge_evm_aggregator::cache",
        stem = %s.pk.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
        "aggregator PK cache HIT"
    );
    Ok(CachedKeygen {
        pk,
        break_points: meta.break_points,
        calculated,
        num_instance: meta.num_instance,
    })
}

/// The cache's protocol, apart from what a slot holds.
///
/// A published slot is loaded. Otherwise this process takes the keygen
/// lock, waiting while another one generates, and looks again: a slot
/// published meanwhile is loaded, not generated twice. Only then does
/// `generate` run, still under the lock, writing the key to the partial
/// path it is given and returning its value and the meta's text; the key
/// is published first, then the meta, and the lock is let go after both.
fn load_or_generate<T>(
    dir: &Path,
    s: &SlotPaths,
    load: impl FnOnce(&SlotPaths) -> anyhow::Result<T>,
    generate: impl FnOnce(&Path) -> anyhow::Result<(T, String)>,
) -> anyhow::Result<T> {
    if published(s) {
        // Nobody writing means every partial file is a leftover; clear them
        // while here, without waiting for a writer to finish.
        if let Ok(Some(_lock)) = try_lock_cache(dir) {
            sweep_and_report(dir);
        }
        return load(s);
    }

    let keygen_lock = lock_cache(dir)?;
    // Without the lock — a filesystem that cannot lock — another writer's
    // partial file may be in the directory, and nothing is swept.
    if keygen_lock.is_some() {
        sweep_and_report(dir);
    }
    // Published by another process while this one waited for the lock.
    if published(s) {
        return load(s);
    }
    clear_unpublished_slot(s)
        .with_context(|| format!("clear unpublished cache slot {}", s.pk.display()))?;

    let writer = writer_tag();
    let key_partial = partial_path(&s.pk, &writer);
    let (value, meta_text) = generate(&key_partial)?;
    publish(&key_partial, &s.pk)?;
    let meta_partial = partial_path(&s.meta, &writer);
    write_synced(&meta_partial, |w| w.write_all(meta_text.as_bytes()))?;
    publish(&meta_partial, &s.meta)?;
    tracing::info!(
        target: "bridge_evm_aggregator::cache",
        stem = %s.pk.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
        "aggregator PK cache MISS -> persisted new slot"
    );
    drop(keygen_lock);
    Ok(value)
}

/// Load `(pk, break_points, calculated, num_instance)` from `cache_dir` if
/// both slot files exist. On cache miss, run the real outer keygen and
/// (if `cache_dir` is `Some`) persist everything for subsequent runs.
///
/// Cost, measured for the withdrawal aggregator at K=21:
/// - cache **miss**: ~20 min wall-clock with the proof, and a ~6.8 GB key
///   written to `cache_dir`
/// - cache **hit**: the key is read back instead (~15-60 s)
///
/// Passing `cache_dir = None` is equivalent to the pre-cache behaviour: full
/// keygen every call, nothing persisted.
pub fn keygen_or_load(
    agg_params: &ParamsKZG<Bn256>,
    base_name: &str,
    config: AggregatorConfig,
    inner_snark: &Snark,
    cache_dir: Option<&Path>,
) -> anyhow::Result<CachedKeygen> {
    let Some(dir) = cache_dir else {
        return Ok(keygen(agg_params, &config, inner_snark));
    };
    std::fs::create_dir_all(dir).with_context(|| format!("create cache dir {}", dir.display()))?;
    let stem = cache_stem(base_name, &config, agg_params, inner_snark);
    let s = slot_paths(dir, &stem);
    load_or_generate(
        dir,
        &s,
        |s| load_slot(agg_params, s, inner_snark, config.universality),
        |key_partial| {
            let k = keygen(agg_params, &config, inner_snark);
            write_synced(key_partial, |w| {
                k.pk.write(w, SerdeFormat::RawBytesUnchecked)
            })?;
            let meta = CachedMeta {
                break_points: k.break_points.clone(),
                calculated: k.calculated.into(),
                num_instance: k.num_instance.clone(),
            };
            let meta_text = serde_json::to_string_pretty(&meta).context("serialize cache meta")?;
            Ok((k, meta_text))
        },
    )
}

/// The real outer keygen, in memory: the key is written by the caller.
fn keygen(
    agg_params: &ParamsKZG<Bn256>,
    config: &AggregatorConfig,
    inner_snark: &Snark,
) -> CachedKeygen {
    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };
    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        config.universality,
    );
    keygen_circuit.expose_previous_instances(false);
    // Bind the inner-circuit VK. Must run before `calculate_params` /
    // `num_instance` so the extra Poseidon gates are counted in the
    // auto-config and the persisted `num_instance` reflects the +1
    // exposed instance.
    crate::vk_binding::expose_vk_digest(&mut keygen_circuit);
    let calculated = keygen_circuit.calculate_params(Some(10));
    let num_instance = keygen_circuit.num_instance();
    let pk = gen_pk(agg_params, &keygen_circuit, None);
    let break_points = keygen_circuit.break_points();
    CachedKeygen {
        pk,
        break_points,
        calculated,
        num_instance,
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the content-addressed cache key. These exercise
    //! `content_hash` directly with hand-crafted bytes — building a real
    //! `Snark` + `ParamsKZG` is prohibitively expensive for a unit test, and
    //! the correctness properties Sergey flagged live entirely at the
    //! hash-preimage layer.
    use super::*;

    fn base_cfg() -> AggregatorConfig {
        AggregatorConfig::default()
    }

    #[test]
    fn content_hash_is_deterministic() {
        let cfg = base_cfg();
        let s_g2 = b"srs-s-g2-bytes";
        let proto = b"protocol-bytes";
        assert_eq!(content_hash(&cfg, s_g2, proto), content_hash(&cfg, s_g2, proto));
    }

    #[test]
    fn content_hash_diverges_on_protocol_change() {
        let cfg = base_cfg();
        let s_g2 = b"srs-s-g2-bytes";
        let a = content_hash(&cfg, s_g2, b"protocol-A");
        let b = content_hash(&cfg, s_g2, b"protocol-B");
        assert_ne!(a, b, "distinct inner protocols must not share a cache slot");
    }

    #[test]
    fn content_hash_diverges_on_srs_change() {
        // The v1 SipHash-only key omitted SRS entirely. A re-bootstrapped SRS
        // under an unchanged base_name would silently serve stale keys. The
        // v2 key must catch this.
        let cfg = base_cfg();
        let proto = b"protocol-bytes";
        let hermez_head = content_hash(&cfg, b"hermez-srs", proto);
        let toxic_head = content_hash(&cfg, b"toxic-waste-srs", proto);
        assert_ne!(
            hermez_head, toxic_head,
            "distinct SRS ceremonies must not share a cache slot"
        );
    }

    #[test]
    fn content_hash_diverges_on_config_change() {
        let s_g2 = b"srs-s-g2-bytes";
        let proto = b"protocol-bytes";
        let base = content_hash(&base_cfg(), s_g2, proto);

        let mut c = base_cfg();
        c.k_outer = base_cfg().k_outer + 1;
        assert_ne!(base, content_hash(&c, s_g2, proto), "k_outer must be in key");

        let mut c = base_cfg();
        c.lookup_bits_outer = base_cfg().lookup_bits_outer + 1;
        assert_ne!(base, content_hash(&c, s_g2, proto), "lookup_bits_outer must be in key");

        let mut c = base_cfg();
        c.universality = match c.universality {
            VerifierUniversality::None => VerifierUniversality::Full,
            _ => VerifierUniversality::None,
        };
        assert_ne!(base, content_hash(&c, s_g2, proto), "universality must be in key");
    }

    #[test]
    fn content_hash_length_prefix_prevents_boundary_ambiguity() {
        // Without length prefixes, ("ab", "cd") and ("a", "bcd") would hash
        // identically. The v2 preimage encodes u32-le length before each
        // variable-length field.
        let cfg = base_cfg();
        let a = content_hash(&cfg, b"ab", b"cd");
        let b = content_hash(&cfg, b"a", b"bcd");
        assert_ne!(a, b, "field lengths must be encoded to disambiguate boundaries");
    }

    /// A fresh directory under the system temp dir, removed by the caller.
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agg_cache_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_partial_slot_file_is_named_after_its_slot_and_writer() {
        assert_eq!(
            partial_path(Path::new("/c/X__v3__ab.pk"), "42-0a1b2c3d"),
            PathBuf::from("/c/X__v3__ab.pk.partial-42-0a1b2c3d")
        );
        assert_eq!(
            partial_path(Path::new("/c/X__v3__ab.meta.json"), "7-0a1b2c3d"),
            PathBuf::from("/c/X__v3__ab.meta.json.partial-7-0a1b2c3d")
        );
    }

    #[test]
    fn sweeping_removes_every_partial_and_nothing_else() {
        let d = scratch("sweep");
        for name in [
            "A.pk.partial-111",
            "A.meta.json.partial-111",
            "B.pk",
            "B.meta.json",
            KEYGEN_LOCK,
        ] {
            std::fs::write(d.join(name), b"x").unwrap();
        }
        let mut removed = sweep_partials(&d);
        removed.sort();
        assert_eq!(removed, vec![
            d.join("A.meta.json.partial-111"),
            d.join("A.pk.partial-111")
        ]);
        assert!(d.join("B.pk").exists() && d.join("B.meta.json").exists());
        assert!(d.join(KEYGEN_LOCK).exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_held_cache_lock_means_a_writer_is_at_work() {
        // `flock(2)` belongs to the open file, so a second open in this
        // process is refused like another process would be — in any PID
        // namespace, which a pid check is not.
        let d = scratch("lock");
        let writer = try_lock_cache(&d)
            .unwrap()
            .expect("a free cache lock is taken");
        assert!(
            try_lock_cache(&d).unwrap().is_none(),
            "nobody may sweep while a writer holds the lock"
        );
        drop(writer);
        assert!(
            try_lock_cache(&d).unwrap().is_some(),
            "the lock is free again"
        );
        assert!(
            lock_cache(&d).unwrap().is_some(),
            "and a waiting writer gets it"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_waiting_writer_gets_the_lock_once_the_holder_lets_go() {
        let d = scratch("wait");
        let holder = try_lock_cache(&d).unwrap().expect("free");
        let dir = d.clone();
        let waiter = std::thread::spawn(move || lock_cache(&dir).unwrap().is_some());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!waiter.is_finished(), "the second writer waits");
        drop(holder);
        assert!(waiter.join().unwrap());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_lock_file_this_user_cannot_write_still_locks() {
        // Another user created `.keygen.lock` in a shared cache. `flock`
        // needs only an open file, so a read-only one has to serve: an
        // unlocked keygen here would be swept from under it by the other
        // user, who can lock.
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("ro");
        let path = d.join(KEYGEN_LOCK);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let held = lock_cache(&d)
            .expect("a read-only lock file is still a lock")
            .expect("and this filesystem locks");
        assert!(
            try_lock_cache(&d).unwrap().is_none(),
            "the read-only open really holds it"
        );
        drop(held);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_lock_file_this_user_cannot_open_is_an_error_not_an_unlocked_keygen() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("noperm");
        let path = d.join(KEYGEN_LOCK);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything: nothing to learn here then.
        if std::fs::File::open(&path).is_ok() {
            std::fs::remove_dir_all(&d).ok();
            return;
        }
        let err = lock_cache(&d).expect_err("an unopenable lock is not a lockless filesystem");
        assert!(format!("{err:#}").contains(KEYGEN_LOCK), "{err:#}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn only_a_filesystem_without_locks_runs_keygen_unlocked() {
        for (errno, unsupported) in [(37, true), (38, true), (95, true), (13, false), (5, false)] {
            assert_eq!(
                locking_unsupported(&std::io::Error::from_raw_os_error(errno)),
                unsupported,
                "errno {errno}"
            );
        }
    }

    #[test]
    fn a_key_with_no_meta_beside_it_is_cleared_before_keygen() {
        // The SDK's `read_pk` panics on a truncated key rather than
        // regenerating it, so an unpublished `.pk` must never be read.
        let d = scratch("clear");
        let s = slot_paths(&d, "S");
        std::fs::write(&s.pk, b"truncated").unwrap();
        clear_unpublished_slot(&s).unwrap();
        assert!(!s.pk.exists(), "a key without its meta was never published");

        std::fs::write(&s.pk, b"pk").unwrap();
        std::fs::write(&s.meta, b"{}").unwrap();
        clear_unpublished_slot(&s).unwrap();
        assert!(s.pk.exists(), "a published slot is left alone");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn publishing_moves_the_finished_file_into_place() {
        let d = scratch("publish");
        let finished = d.join("S.pk.partial-1");
        let slot = d.join("S.pk");
        std::fs::write(&slot, b"old").unwrap();
        std::fs::write(&finished, b"new").unwrap();
        publish(&finished, &slot).unwrap();
        assert_eq!(std::fs::read(&slot).unwrap(), b"new");
        assert!(!finished.exists());
        std::fs::remove_dir_all(&d).ok();
    }

    /// A slot's two files, published by hand.
    fn publish_by_hand(s: &SlotPaths, key: &[u8]) {
        std::fs::write(&s.pk, key).unwrap();
        std::fs::write(&s.meta, b"{}").unwrap();
    }

    #[test]
    fn a_published_slot_is_loaded_and_never_generated() {
        let d = scratch("hit");
        let s = slot_paths(&d, "S");
        publish_by_hand(&s, b"key");
        let got = load_or_generate(
            &d,
            &s,
            |s| Ok(std::fs::read(&s.pk)?),
            |_| panic!("a published slot is not generated again"),
        )
        .unwrap();
        assert_eq!(got, b"key");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_slot_published_while_waiting_for_the_lock_is_loaded_not_generated_again() {
        // Two cold runs on one cache: the second waits for the first's lock,
        // and must find the first's slot once it has it. Generating it again
        // would write a second ~6.8 GB key beside the first.
        let d = scratch("raced");
        let holder = try_lock_cache(&d).unwrap().expect("free");
        let dir = d.clone();
        let waiter = std::thread::spawn(move || {
            let s = slot_paths(&dir, "S");
            load_or_generate(
                &dir,
                &s,
                |s| Ok(std::fs::read(&s.pk)?),
                |_| Ok((b"generated twice".to_vec(), "{}".to_string())),
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !waiter.is_finished(),
            "the second writer waits for the lock"
        );
        publish_by_hand(&slot_paths(&d, "S"), b"the first writer's key");
        drop(holder);
        assert_eq!(waiter.join().unwrap().unwrap(), b"the first writer's key");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_key_is_generated_under_the_lock_and_published_with_its_meta() {
        let d = scratch("miss");
        let s = slot_paths(&d, "S");
        let got = load_or_generate(
            &d,
            &s,
            |_| panic!("nothing is published yet"),
            |partial| {
                assert!(
                    try_lock_cache(&d).unwrap().is_none(),
                    "the cache lock is held while the key is generated"
                );
                assert!(
                    partial
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("S.pk.partial-"),
                    "{}",
                    partial.display()
                );
                write_synced(partial, |w| w.write_all(b"fresh key"))?;
                Ok((7, "{\"meta\":1}".to_string()))
            },
        )
        .unwrap();
        assert_eq!(got, 7);
        assert_eq!(std::fs::read(&s.pk).unwrap(), b"fresh key");
        assert_eq!(std::fs::read_to_string(&s.meta).unwrap(), "{\"meta\":1}");
        assert!(
            sweep_partials(&d).is_empty(),
            "nothing is left half-written"
        );
        assert!(
            try_lock_cache(&d).unwrap().is_some(),
            "and the lock is let go once the slot is published"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_failed_keygen_publishes_nothing() {
        let d = scratch("failed");
        let s = slot_paths(&d, "S");
        let err = load_or_generate::<()>(
            &d,
            &s,
            |_| panic!("nothing is published"),
            |_| anyhow::bail!("keygen failed"),
        )
        .expect_err("the failure is the caller's");
        assert!(format!("{err:#}").contains("keygen failed"));
        assert!(!s.pk.exists() && !s.meta.exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_write_whose_last_buffer_cannot_be_flushed_is_an_error() {
        // `BufWriter` flushes its last buffer on drop and throws the error
        // away. A key whose tail never reached the disk must not be
        // published as whole.
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("/dev/full");
        let err = fill_and_sync(full, |w| w.write_all(b"the tail of a key"))
            .expect_err("ENOSPC on the final flush is reported");
        assert_eq!(err.raw_os_error(), Some(28), "{err}");
    }

    #[test]
    fn a_partial_file_is_never_written_over() {
        // Without the lock — a filesystem that cannot lock — two writers in
        // different PID namespaces can share a pid. Neither may truncate the
        // other's partial file.
        let d = scratch("create_new");
        let p = d.join("S.pk.partial-1");
        std::fs::write(&p, b"someone else's").unwrap();
        write_synced(&p, |w| w.write_all(b"mine")).expect_err("the file is not ours");
        assert_eq!(std::fs::read(&p).unwrap(), b"someone else's");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn two_writers_name_their_partial_files_apart() {
        assert_ne!(
            writer_tag(),
            writer_tag(),
            "a pid alone is not unique across namespaces"
        );
        assert!(writer_tag().starts_with(&format!("{}-", std::process::id())));
    }

    #[test]
    fn an_unreadable_published_key_is_an_error_not_a_keygen() {
        // `gen_pk(.., Some(path))` regenerates a key it cannot open — in
        // place, under the published name, without the lock — and panics on
        // a truncated one.
        let d = scratch("read");
        let params = AggregationConfigParams {
            degree: 4,
            lookup_bits: 3,
            ..Default::default()
        };
        let err = read_slot_key(&d.join("missing.pk"), params).expect_err("missing");
        assert!(format!("{err:#}").contains("missing.pk"), "{err:#}");
        let truncated = d.join("truncated.pk");
        std::fs::write(&truncated, [0x02, 0x01]).unwrap();
        let err = read_slot_key(&truncated, params).expect_err("truncated");
        assert!(
            format!("{err:#}").contains(".meta.json"),
            "says what to remove: {err:#}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_key_reaches_the_cache_only_through_a_synced_partial_file() {
        // `gen_pk(.., Some(path))` writes straight to `path` through a
        // `BufWriter` whose last flush error it drops, syncs nothing, and
        // regenerates over a key it cannot open. The behaviour around it is
        // tested above through `load_or_generate`; this pins that the real
        // keygen goes through it.
        let src = include_str!("aggregator_cache.rs");
        let src = &src[..src.find("#[cfg(test)]").unwrap()];
        let code: Vec<&str> = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        assert_eq!(
            code.iter().filter(|l| l.contains("gen_pk(")).count(),
            1,
            "one keygen"
        );
        assert!(
            src.contains("gen_pk(agg_params, &keygen_circuit, None)"),
            "and it writes no file of its own"
        );
        assert!(
            src.contains("load_or_generate(\n"),
            "keygen_or_load goes through the protocol"
        );
        // Leftovers are told from live writes by the cache lock, never by a
        // pid: a pid means nothing outside its own namespace.
        assert!(!src.contains("/proc/"), "no pid-based liveness");
        // Every sweep happens under the lock: the writer's, or one the
        // cache-hit path takes without waiting.
        assert_eq!(src.matches("sweep_partials(dir)").count(), 1);
        assert_eq!(src.matches("sweep_and_report(dir);").count(), 2);
        assert!(src.contains(
            "if let Ok(Some(_lock)) = try_lock_cache(dir) {\n            sweep_and_report(dir);"
        ));
        assert!(src.contains("if keygen_lock.is_some() {\n        sweep_and_report(dir);"));
        // A rename is durable once its directory is.
        let publish = &src[src.find("fn publish(").unwrap()..];
        let publish = &publish[..publish.find("\n}\n").unwrap()];
        assert!(publish.contains("sync_dir("), "{publish}");
    }
}
