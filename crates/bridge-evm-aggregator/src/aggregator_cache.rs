//! Persistent PK cache for the outer SHPLONK aggregator keygen.
//!
//! `aggregate_and_prove` / `aggregate_inner` / `generate_yul_verifier` all
//! previously called `gen_pk(agg_params, &circuit, None)` on every
//! invocation, forcing a full outer keygen at K=21. Every rerun of
//! `export-inner-aggregator` and every runtime call to `aggregate-proof` paid
//! that cost from scratch.
//!
//! This module memoises the outer
//! `(pk, break_points, calculated, num_instance)` bundle on disk, keyed by a
//! **content hash** over the inputs that actually determine the outer PK:
//! `AggregatorConfig`, the SRS `s_g2` head, and `bincode(inner_snark.protocol)`
//! (a stable function of the inner VK).
//!
//! Cache slot layout under `<cache_dir>`:
//! - `<stem>.pk`         -- proving key (halo2's `RawBytes` format, ~6.85 GB
//!   for the withdrawal aggregator @ K=21)
//! - `<stem>.meta.json`  -- break_points + calculated params + num_instance,
//!   and the key's length
//!
//! Both are written under a `.partial-<writer>` name and synced, then
//! renamed into place, the key first: a slot whose meta exists has a whole
//! key, after a crash of the host too. The stem of the slot a run used comes
//! back in [`CachedKeygen::slot`], and `aggregate-proof --slot-report` writes
//! it where its caller asks.
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
        halo2curves::{bn256::{Bn256, Fr, G1Affine}, serde::SerdeObject},
        plonk::{Circuit, ProvingKey},
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
    /// The stem of the pk-cache slot this came from or went to; `None`
    /// without a cache.
    pub slot: Option<String>,
}

/// A slot inside `cache_dir`: its stem and its two files (`.pk` +
/// `.meta.json`).
struct SlotPaths {
    stem: String,
    pk: PathBuf,
    meta: PathBuf,
}

/// Everything a slot holds beside the key. Uses local mirror structs instead
/// of relying on `AggregationConfigParams: Serialize` -- serde derive status
/// is version-dependent, and mirroring costs 20 lines.
#[derive(Serialize, Deserialize)]
struct CachedMeta {
    break_points: Vec<Vec<usize>>,
    calculated: MirrorConfigParams,
    num_instance: Vec<usize>,
    /// The key file's length, checked before the key is read. Absent from a
    /// slot an older `aggregate-proof` published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pk_bytes: Option<u64>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
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
    // `v3` here and in the hash's domain tag: bump both whenever what a slot
    // holds stops matching what this code builds — the outer circuit, its
    // config, the files' format — so that no older slot is ever loaded.
    format!("{base_name}__v3__{}", hex::encode(&hash[..16]))
}

fn slot_paths(cache_dir: &Path, stem: &str) -> SlotPaths {
    SlotPaths {
        stem: stem.to_string(),
        pk: cache_dir.join(format!("{stem}.pk")),
        meta: cache_dir.join(format!("{stem}.meta.json")),
    }
}

/// Suffix of a slot file still being written: `<stem>.pk.partial-<writer>`
/// and `<stem>.meta.json.partial-<writer>`, renamed into place only once
/// complete and synced. A writer killed mid-way — a caller's timeout, the OOM
/// killer — leaves one of these and never a truncated `<stem>.pk`, which
/// every later run would fail on until someone deleted it by hand. A write
/// that fails removes its own; a leftover is removed by the next process
/// that holds [`KEYGEN_LOCK`]. `ackinacki-bridge`'s preflight looks for this
/// suffix to name it.
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
/// or one killed between the two renames — or it was set aside as damaged,
/// and nothing reads it. Removed rather than left, so the keygen that
/// follows has its room, with a set-aside meta. A meta that cannot be looked
/// at is an error, not an absent one: the key beside it may be good.
fn clear_unpublished_slot(s: &SlotPaths) -> std::io::Result<()> {
    if s.pk.try_exists()? && !s.meta.try_exists()? {
        std::fs::remove_file(&s.pk)?;
    }
    // A set-aside meta is a few hundred bytes: one that cannot be removed —
    // another user's, in a sticky directory — is no reason to stop.
    let aside = damaged_meta_path(s);
    if let Err(e) = std::fs::remove_file(&aside) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                target: "bridge_evm_aggregator::cache",
                path = %aside.display(),
                error = %e,
                "could not remove a damaged slot's set-aside meta"
            );
        }
    }
    Ok(())
}

/// Create `path` — never over an existing file — fill it through
/// `write`, and sync it. A failed write removes the file: it is this
/// writer's, and a disk that filled up wants its room back for the retry.
fn write_synced(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    fill_and_sync(file, write)
        .inspect_err(|_| remove_own(path))
        .with_context(|| format!("write {}", path.display()))
}

/// Remove a file this writer created and gives up on. Nothing to do if that
/// fails: the next holder of the keygen lock sweeps partial files.
fn remove_own(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                target: "bridge_evm_aggregator::cache",
                path = %path.display(),
                error = %e,
                "could not remove a partial cache file after a failed write"
            );
        }
    }
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

/// Write `pk` to `path` through [`write_synced`] and return the file's
/// length, which the slot's meta records.
fn write_key(path: &Path, pk: &ProvingKey<G1Affine>) -> anyhow::Result<u64> {
    write_synced(path, |w| pk.write(w, SerdeFormat::RawBytes))?;
    Ok(std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len())
}

/// Suffix a slot's meta is renamed to when its key or meta turns out
/// damaged ([`set_aside_damaged`]): the slot is then unpublished, the next
/// keygen for it clears it, and `ackinacki-bridge`'s preflight refuses a
/// withdrawal while it is there, naming both files.
pub const DAMAGED_SUFFIX: &str = ".damaged";

/// The context of a read error that means a slot's bytes are wrong, not
/// that they could not be read: [`read_slot`] sets such a slot aside.
#[derive(Debug)]
struct DamagedSlot(String);

impl std::fmt::Display for DamagedSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DamagedSlot {}

/// Whether a failed key read means the bytes are wrong — short, or not a
/// key — rather than that they could not be read: a permission, an I/O
/// error, after which removing the slot would throw a good key away.
fn damaged(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData
    )
}

/// Read a published key. `recorded_len` is its length as the slot's meta
/// records it (`None` for a slot an older `aggregate-proof` published), and
/// a key of another length is refused before the read.
///
/// Read in `RawBytes`, the bytes `RawBytesUnchecked` reads too, because the
/// unchecked read unwraps every short read: a key cut short panicked
/// instead of saying which file to remove. The check it adds — every field
/// element below its modulus — costs one comparison per element, and read
/// the real ~6.85 GB key no slower. It is not a check of the content: wrong
/// bytes that are not field elements are refused as damage, but wrong bytes
/// that are — and points off the curve — read. The proof made with such a
/// key fails the check the SDK runs on every proof it makes, and
/// `aggregate_and_prove_cached` says so, naming the slot's files; that slot
/// is not set aside, for a wrong inner snark fails the same check. Not
/// through `gen_pk(.., Some(path))` either: that runs a whole keygen when
/// the file cannot be opened and writes the result over the published
/// name, without the lock.
fn read_slot_key<C: Circuit<Fr>>(
    path: &Path,
    params: C::Params,
    recorded_len: Option<u64>,
) -> anyhow::Result<ProvingKey<G1Affine>> {
    let file = File::open(path)
        .with_context(|| format!("open cached aggregator key {}", path.display()))?;
    if let Some(want) = recorded_len {
        let got = file
            .metadata()
            .with_context(|| format!("stat cached aggregator key {}", path.display()))?
            .len();
        if got != want {
            return Err(anyhow::Error::new(DamagedSlot(format!(
                "cached aggregator key {} is damaged: {got} bytes, and its .meta.json records \
                 {want}",
                path.display()
            ))));
        }
    }
    let mut r = BufReader::with_capacity(IO_BUFFER, file);
    ProvingKey::read::<_, C>(&mut r, SerdeFormat::RawBytes, params).map_err(|e| {
        if damaged(&e) {
            anyhow::Error::new(e).context(DamagedSlot(format!(
                "cached aggregator key {} is damaged",
                path.display()
            )))
        } else {
            anyhow::Error::new(e).context(format!("read cached aggregator key {}", path.display()))
        }
    })
}

/// Read a slot's meta. Bytes that are not UTF-8 are as damaged as JSON cut
/// short, and say so.
fn read_meta(path: &Path) -> anyhow::Result<CachedMeta> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read cache meta {}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        anyhow::Error::new(e).context(DamagedSlot(format!(
            "cache meta {} is damaged",
            path.display()
        )))
    })
}

/// Whether a slot is published: its meta is renamed into place after its
/// key, so a meta means a whole key.
fn published(s: &SlotPaths) -> anyhow::Result<bool> {
    let exists = |p: &Path| {
        p.try_exists()
            .with_context(|| format!("look for cache slot file {}", p.display()))
    };
    Ok(exists(&s.pk)? && exists(&s.meta)?)
}

/// Read a published slot's meta, then its key with the `params` the meta
/// gives. `held`: this process holds the keygen lock already. A slot an
/// older `aggregate-proof` published records no key length; once its key
/// has read back whole, the length is added ([`record_key_length`]), so
/// that the next preflight can check it. A slot found damaged is set aside
/// ([`set_aside_damaged`]), and the error says what is left to do.
fn read_slot<C: Circuit<Fr>>(
    s: &SlotPaths,
    held: bool,
    params: impl FnOnce(&CachedMeta) -> C::Params,
) -> anyhow::Result<(ProvingKey<G1Affine>, CachedMeta)> {
    // The meta as read: what is rewritten or set aside below has to be it.
    let read = file_id(&s.meta);
    let damaged = |e: anyhow::Error| {
        if e.downcast_ref::<DamagedSlot>().is_none() {
            e
        } else if set_aside_damaged(s, held, read) {
            e.context(format!(
                "set aside as {}: the next run that needs this slot generates it again, and there \
                 is nothing to remove",
                damaged_meta_path(s).display()
            ))
        } else {
            e.context(format!(
                "remove {} and {} to have both generated again",
                s.pk.display(),
                s.meta.display()
            ))
        }
    };
    let mut meta = read_meta(&s.meta).map_err(damaged)?;
    let pk = read_slot_key::<C>(&s.pk, params(&meta), meta.pk_bytes).map_err(damaged)?;
    if meta.pk_bytes.is_none() {
        if let Ok(md) = std::fs::metadata(&s.pk) {
            meta.pk_bytes = Some(md.len());
            record_key_length(s, &meta, held, read);
        }
    }
    Ok((pk, meta))
}

/// A file's device and inode: which file a path named when it was read.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// The keygen lock for a write to a published slot, which only a holder of
/// the lock makes: `Some(None)` when this process already holds it
/// (`held`), `Some(Some(lock))` when it is taken now, without waiting, and
/// `None` when another process holds it or it cannot be taken.
fn write_lock(dir: &Path, held: bool) -> Option<Option<File>> {
    if held {
        return Some(None);
    }
    match try_lock_cache(dir) {
        Ok(Some(lock)) => Some(Some(lock)),
        _ => None,
    }
}

/// Publish `meta` — the slot's own, with the key's length added — over the
/// slot's meta (`read` is the one read), with the old one's permissions.
/// Only under the keygen lock ([`write_lock`]): with another writer at
/// work, a later run adds it. And only over a meta this user's uid and gid
/// own: a new file belongs to whoever writes it, and another user's meta
/// rewritten by a container running as root could become unreadable to that
/// user. Best effort; without it, preflight only cannot check the length.
fn record_key_length(s: &SlotPaths, meta: &CachedMeta, held: bool, read: Option<(u64, u64)>) {
    use std::os::unix::fs::MetadataExt as _;

    let skip = |why: &str| {
        tracing::info!(
            target: "bridge_evm_aggregator::cache",
            path = %s.meta.display(),
            "not adding the key's length to the slot's meta: {why}"
        );
    };
    let Some(dir) = s.meta.parent() else {
        return;
    };
    let Some(_lock) = write_lock(dir, held) else {
        return skip("another aggregate-proof holds the cache lock; a later run adds it");
    };
    let Ok(old) = std::fs::metadata(&s.meta) else {
        return;
    };
    if read != Some((old.dev(), old.ino())) {
        return skip("it changed since it was read");
    }
    let partial = partial_path(&s.meta, &writer_tag());
    let recorded = serde_json::to_string_pretty(meta)
        .context("serialize cache meta")
        .and_then(|text| write_synced(&partial, |w| w.write_all(text.as_bytes())))
        .and_then(|()| {
            let mine = std::fs::metadata(&partial)
                .with_context(|| format!("stat {}", partial.display()))?;
            if (mine.uid(), mine.gid()) != (old.uid(), old.gid()) {
                remove_own(&partial);
                skip("it belongs to another user or group");
                return Ok(());
            }
            std::fs::set_permissions(&partial, old.permissions())
                .with_context(|| format!("chmod {}", partial.display()))?;
            publish(&partial, &s.meta)
        });
    if let Err(e) = recorded {
        remove_own(&partial);
        tracing::warn!(
            target: "bridge_evm_aggregator::cache",
            path = %s.meta.display(),
            error = %format!("{e:#}"),
            "could not add the key's length to the slot's meta"
        );
    }
}

/// Rename a damaged slot's meta — the one `read`, still there — to
/// `<stem>.meta.json`[`DAMAGED_SUFFIX`], under the keygen lock
/// ([`write_lock`]). The slot is unpublished then: the next run that needs
/// it clears both files and generates it again, instead of every
/// `--allow-retry` failing here after its capture and Circuit-4 proof.
/// `false`: it stays published, with a warning.
fn set_aside_damaged(s: &SlotPaths, held: bool, read: Option<(u64, u64)>) -> bool {
    let left = |why: String| {
        tracing::warn!(
            target: "bridge_evm_aggregator::cache",
            path = %s.meta.display(),
            "the damaged slot stays published ({why}): remove {} and {}",
            s.pk.display(),
            s.meta.display()
        );
        false
    };
    let Some(dir) = s.meta.parent() else {
        return false;
    };
    let Some(_lock) = write_lock(dir, held) else {
        return left("another aggregate-proof holds the cache lock".to_string());
    };
    if read.is_none() || file_id(&s.meta) != read {
        return left("its meta changed since it was read".to_string());
    }
    match std::fs::rename(&s.meta, damaged_meta_path(s)).and_then(|()| sync_dir(dir)) {
        Ok(()) => true,
        Err(e) => left(format!("it could not be renamed: {e}")),
    }
}

/// Where [`set_aside_damaged`] puts a slot's meta.
fn damaged_meta_path(s: &SlotPaths) -> PathBuf {
    let mut name = s.meta.file_name().unwrap_or_default().to_os_string();
    name.push(DAMAGED_SUFFIX);
    s.meta.with_file_name(name)
}

/// Read a published slot: the key, and the rest from the meta.
fn load_slot(
    agg_params: &ParamsKZG<Bn256>,
    s: &SlotPaths,
    held: bool,
    inner_snark: &Snark,
    universality: VerifierUniversality,
) -> anyhow::Result<CachedKeygen> {
    // Build a "post-keygen shape" circuit for the params the key is read
    // with. No synthesis runs. We omit `expose_previous_instances` /
    // `calculate_params` deliberately: both `num_instance` and
    // `break_points` are recovered from `meta` so re-deriving them here
    // would be pure overhead.
    let (pk, meta) = read_slot::<AggregationCircuit>(s, held, |meta| {
        AggregationCircuit::new::<SHPLONK>(
            CircuitBuilderStage::Keygen,
            meta.calculated.into(),
            agg_params,
            vec![inner_snark.clone()],
            universality,
        )
        .params()
    })?;
    tracing::info!(
        target: "bridge_evm_aggregator::cache",
        stem = %s.stem,
        "aggregator PK cache HIT"
    );
    Ok(CachedKeygen {
        pk,
        break_points: meta.break_points,
        calculated: meta.calculated.into(),
        num_instance: meta.num_instance,
        slot: None,
    })
}

/// The cache's protocol, apart from what a slot holds.
///
/// A published slot is loaded. Otherwise this process takes the keygen
/// lock, waiting while another one generates, and looks again: a slot
/// published meanwhile is loaded, not generated twice. Only then does
/// `generate` run, still under the lock, writing the key to the partial
/// path it is given and returning its value and the meta's text; the meta
/// is written beside it, then the key is published, then the meta, and the
/// lock is let go after both.
fn load_or_generate<T>(
    dir: &Path,
    s: &SlotPaths,
    load: impl FnOnce(&SlotPaths, bool) -> anyhow::Result<T>,
    generate: impl FnOnce(&Path) -> anyhow::Result<(T, String)>,
) -> anyhow::Result<T> {
    if published(s)? {
        // Nobody writing means every partial file is a leftover; clear them
        // while here, without waiting for a writer to finish.
        if let Ok(Some(_lock)) = try_lock_cache(dir) {
            sweep_and_report(dir);
        }
        return load(s, false);
    }

    let keygen_lock = lock_cache(dir)?;
    // Without the lock — a filesystem that cannot lock — another writer's
    // partial file may be in the directory, and nothing is swept.
    if keygen_lock.is_some() {
        sweep_and_report(dir);
    }
    // Published by another process while this one waited for the lock.
    if published(s)? {
        return load(s, keygen_lock.is_some());
    }
    clear_unpublished_slot(s)
        .with_context(|| format!("clear unpublished cache slot {}", s.pk.display()))?;

    let writer = writer_tag();
    let key_partial = partial_path(&s.pk, &writer);
    let meta_partial = partial_path(&s.meta, &writer);
    let value = write_slot(&key_partial, &meta_partial, generate).inspect_err(|_| {
        // This writer's own names: removed now, so a retry has their room.
        remove_own(&key_partial);
        remove_own(&meta_partial);
    })?;
    publish_slot(&keygen_lock, s, &key_partial, &meta_partial)?;
    drop(keygen_lock);
    Ok(value)
}

/// Run `generate`, which writes the key to `key_partial`, and write the
/// meta it returns to `meta_partial`. Both are whole and synced when this
/// returns, and neither is published: a meta that does not fit on the disk
/// fails here, before the key takes its slot's name.
fn write_slot<T>(
    key_partial: &Path,
    meta_partial: &Path,
    generate: impl FnOnce(&Path) -> anyhow::Result<(T, String)>,
) -> anyhow::Result<T> {
    let (value, meta_text) = generate(key_partial)?;
    write_synced(meta_partial, |w| w.write_all(meta_text.as_bytes()))?;
    Ok(value)
}

/// Publish a written slot, the key first: a meta means a whole key. `_held`
/// is the keygen lock, borrowed so that it cannot be let go first.
fn publish_slot(
    _held: &Option<File>,
    s: &SlotPaths,
    key_partial: &Path,
    meta_partial: &Path,
) -> anyhow::Result<()> {
    publish(key_partial, &s.pk)?;
    publish(meta_partial, &s.meta)?;
    tracing::info!(
        target: "bridge_evm_aggregator::cache",
        stem = %s.stem,
        "aggregator PK cache MISS -> persisted new slot"
    );
    Ok(())
}

/// Load `(pk, break_points, calculated, num_instance)` from `cache_dir` if
/// both slot files exist. On cache miss, run the real outer keygen and
/// (if `cache_dir` is `Some`) persist everything for subsequent runs.
///
/// The slot's stem comes back in [`CachedKeygen::slot`].
///
/// Measured for the withdrawal aggregator at K=21: a cold `aggregate-proof`
/// — this keygen, writing the ~6.85 GB key to `cache_dir`, and the proof —
/// took ~2 min on the test host. A warm one reads the key back instead.
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
        |s, held| load_slot(agg_params, s, held, inner_snark, config.universality),
        |key_partial| {
            let k = keygen(agg_params, &config, inner_snark);
            let meta_text = write_generated(key_partial, &k)?;
            Ok((k, meta_text))
        },
    )
    .map(|k| CachedKeygen {
        slot: Some(stem),
        ..k
    })
}

/// Write a freshly generated key to `key_partial` and return the text of
/// its meta, which records the key's length.
fn write_generated(key_partial: &Path, k: &CachedKeygen) -> anyhow::Result<String> {
    let pk_bytes = write_key(key_partial, &k.pk)?;
    let meta = CachedMeta {
        break_points: k.break_points.clone(),
        calculated: k.calculated.into(),
        num_instance: k.num_instance.clone(),
        pk_bytes: Some(pk_bytes),
    };
    serde_json::to_string_pretty(&meta).context("serialize cache meta")
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
        slot: None,
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the content-addressed cache key. These exercise
    //! `content_hash` directly with hand-crafted bytes — building a real
    //! `Snark` + `ParamsKZG` is prohibitively expensive for a unit test, and
    //! the correctness properties Sergey flagged live entirely at the
    //! hash-preimage layer.
    use halo2_base::gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams};

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
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(lock_cache(&dir).unwrap().is_some()));
        blocked_on(&d.join(KEYGEN_LOCK));
        assert!(
            result.try_recv().is_err(),
            "the second writer waits while the first holds the lock"
        );
        drop(holder);
        assert!(result
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the wait ends once the holder lets go"));
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
        // An unpublished `.pk` may be cut short anywhere: it is never read,
        // and its room is wanted for the keygen.
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
            |s, _| Ok(std::fs::read(&s.pk)?),
            |_| panic!("a published slot is not generated again"),
        )
        .unwrap();
        assert_eq!(got, b"key");
        std::fs::remove_dir_all(&d).ok();
    }

    /// Wait until some thread or process is blocked on the `flock` of
    /// `path`: `/proc/locks` lists a waiter with `->` before its lock.
    fn blocked_on(path: &Path) {
        use std::os::unix::fs::MetadataExt;
        let inode = format!(":{} ", std::fs::metadata(path).unwrap().ino());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let locks = std::fs::read_to_string("/proc/locks").expect("/proc/locks");
            if locks
                .lines()
                .any(|l| l.contains("-> FLOCK") && l.contains(&inode))
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nobody waits for {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_slot_published_while_waiting_for_the_lock_is_loaded_not_generated_again() {
        // Two cold runs on one cache: the second waits for the first's lock,
        // and must find the first's slot once it has it. Generating it again
        // would write a second ~6.85 GB key beside the first.
        let d = scratch("raced");
        let holder = try_lock_cache(&d).unwrap().expect("free");
        let dir = d.clone();
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let s = slot_paths(&dir, "S");
            done.send(load_or_generate(
                &dir,
                &s,
                |s, _| Ok(std::fs::read(&s.pk)?),
                |_| Ok((b"generated twice".to_vec(), "{}".to_string())),
            ))
        });
        // Not a sleep: the waiter is past its first look at the slot only
        // once it is blocked on the lock.
        blocked_on(&d.join(KEYGEN_LOCK));
        publish_by_hand(&slot_paths(&d, "S"), b"the first writer's key");
        drop(holder);
        let got = result
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the waiter finishes once the lock is free");
        assert_eq!(got.unwrap(), b"the first writer's key");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_key_is_generated_under_the_lock_and_published_with_its_meta() {
        let d = scratch("miss");
        let s = slot_paths(&d, "S");
        let got = load_or_generate(
            &d,
            &s,
            |_, _| panic!("nothing is published yet"),
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
            |_, _| panic!("nothing is published"),
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
        let err = read_slot_key::<AggregationCircuit>(&d.join("missing.pk"), params, None)
            .expect_err("missing");
        assert!(format!("{err:#}").contains("missing.pk"), "{err:#}");
        let truncated = d.join("truncated.pk");
        std::fs::write(&truncated, [0x02, 0x01]).unwrap();
        let err =
            read_slot_key::<AggregationCircuit>(&truncated, params, None).expect_err("not a key");
        assert!(
            format!("{err:#}").contains("truncated.pk is damaged"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_failed_write_removes_its_own_partial_file() {
        // A disk that filled up mid-write: the half-written key would hold
        // that space until someone removed it, and the retry's preflight
        // would refuse for want of it.
        let d = scratch("enospc");
        let p = d.join("S.pk.partial-1-0a1b2c3d");
        write_synced(&p, |w| {
            w.write_all(b"the first half")?;
            Err(std::io::Error::from_raw_os_error(28))
        })
        .expect_err("ENOSPC");
        assert!(!p.exists(), "the partial file is gone with the failure");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_keygen_that_fails_after_writing_leaves_no_partial_file() {
        let d = scratch("failed_late");
        let s = slot_paths(&d, "S");
        load_or_generate::<()>(
            &d,
            &s,
            |_, _| panic!("nothing is published"),
            |partial| {
                std::fs::write(partial, b"half a key")?;
                anyhow::bail!("killed by a failure after the write")
            },
        )
        .expect_err("the failure is the caller's");
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(PARTIAL_SUFFIX))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_meta_that_cannot_be_looked_at_keeps_its_key() {
        // `exists()` reads any error as "absent": a meta whose `stat` fails
        // — here a symlink to itself, `ELOOP` — would make a good key look
        // unpublished and have it removed.
        let d = scratch("eloop");
        let s = slot_paths(&d, "S");
        std::fs::write(&s.pk, b"a good key").unwrap();
        std::os::unix::fs::symlink(&s.meta, &s.meta).unwrap();
        clear_unpublished_slot(&s).expect_err("the meta could not be checked");
        assert!(s.pk.exists(), "and the key stays");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A real proving key, small — the multiply circuit at k = 8 — and the
    /// params it is read back with.
    fn small_key() -> (ProvingKey<G1Affine>, BaseCircuitParams) {
        let k = 8;
        let srs = ParamsKZG::<Bn256>::setup(k as u32, rand::rngs::OsRng);
        let (circuit, _) =
            crate::multiply::build_multiply_circuit(false, k, k - 1, Fr::from(3), Fr::from(5));
        (gen_pk(&srs, &circuit, None), circuit.params())
    }

    #[test]
    fn a_key_written_to_the_cache_reads_back_whole() {
        let d = scratch("roundtrip");
        let (pk, params) = small_key();
        let path = d.join("S.pk");
        let len = write_key(&path, &pk).unwrap();
        assert_eq!(len, write_key_len(&path), "the length the meta records");
        write_key(&path, &pk).expect_err("never over an existing file");
        let read = read_slot_key::<BaseCircuitBuilder<Fr>>(&path, params, Some(len)).unwrap();
        assert_eq!(
            read.to_bytes(SerdeFormat::RawBytes),
            pk.to_bytes(SerdeFormat::RawBytes)
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_key_cut_short_is_an_error_not_a_panic() {
        // A key whose header is whole and whose body is not: what a copy
        // cut short, or a filesystem that acknowledged a sync it did not
        // do, leaves. The unchecked read unwraps every short read.
        let d = scratch("short");
        let (pk, params) = small_key();
        let path = d.join("S.pk");
        write_key(&path, &pk).unwrap();
        let whole = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(whole / 2)
            .unwrap();
        let err = read_slot_key::<BaseCircuitBuilder<Fr>>(&path, params.clone(), None)
            .expect_err("half a key is not a key");
        let msg = format!("{err:#}");
        assert!(msg.contains("S.pk is damaged"), "{msg}");
        // A slot this version published records the length: refused before
        // the read, by sizes.
        let err = read_slot_key::<BaseCircuitBuilder<Fr>>(&path, params, Some(whole))
            .expect_err("shorter than recorded");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&format!("{} bytes", whole / 2))
                && msg.contains(&format!("records {whole}")),
            "{msg}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// The length of the key file at `path`.
    fn write_key_len(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    #[test]
    #[ignore = "reads a real ~6.85 GB slot: AGGREGATOR_SLOT=<cache_dir>/<stem>"]
    fn a_published_slot_on_this_host_reads_back() {
        // Slots published before the key was read in the checked format
        // must read in it too, or every warm run would fail after its burn.
        let stem = PathBuf::from(std::env::var("AGGREGATOR_SLOT").expect("AGGREGATOR_SLOT"));
        let meta = read_meta(&stem.with_extension("meta.json")).unwrap();
        let started = std::time::Instant::now();
        let pk = read_slot_key::<AggregationCircuit>(
            &stem.with_extension("pk"),
            meta.calculated.into(),
            meta.pk_bytes,
        )
        .unwrap();
        eprintln!(
            "read in {:?}; {} fixed commitments",
            started.elapsed(),
            pk.get_vk().fixed_commitments().len()
        );
    }

    #[test]
    fn a_key_longer_than_its_meta_records_is_refused_too() {
        let d = scratch("long");
        let (pk, params) = small_key();
        let path = d.join("S.pk");
        let len = write_key(&path, &pk).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"tail").unwrap();
        let err = read_slot_key::<BaseCircuitBuilder<Fr>>(&path, params, Some(len))
            .expect_err("longer than recorded");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&format!("{} bytes", len + 4)) && msg.contains(&format!("records {len}")),
            "{msg}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_damaged_slot_is_set_aside_and_cleared_by_the_next_keygen() {
        // Every `--allow-retry` would otherwise pass preflight and fail here
        // again, after its capture and Circuit-4 proof.
        let d = scratch("aside");
        let s = slot_paths(&d, "S");
        let (pk, params) = small_key();
        let len = write_key(&s.pk, &pk).unwrap();
        std::fs::write(&s.meta, OLD_META).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&s.pk)
            .unwrap()
            .set_len(len / 2)
            .unwrap();
        let err = read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params)
            .err()
            .expect("cut short");
        assert!(format!("{err:#}").contains("is damaged"), "{err:#}");
        let aside = d.join(format!("S.meta.json{DAMAGED_SUFFIX}"));
        assert!(
            !s.meta.exists() && aside.exists(),
            "the slot is unpublished"
        );
        assert!(!published(&s).unwrap());
        clear_unpublished_slot(&s).unwrap();
        assert!(
            !s.pk.exists() && !aside.exists(),
            "and cleared before a keygen"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_slot_is_set_aside_for_a_broken_meta_and_for_a_key_that_is_not_one() {
        let (_, params) = small_key();
        for (key, meta) in [
            (&b"key"[..], &b"{\"break_po"[..]),
            (&[0x02, 0x01][..], OLD_META),
        ] {
            let d = scratch("aside_kinds");
            let s = slot_paths(&d, "S");
            std::fs::write(&s.pk, key).unwrap();
            std::fs::write(&s.meta, meta).unwrap();
            let err = read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params.clone())
                .err()
                .expect("damaged");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("set aside as") && msg.contains("nothing to remove"),
                "{msg}"
            );
            assert!(!s.meta.exists() && damaged_meta_path(&s).exists());
            std::fs::remove_dir_all(&d).ok();
        }
    }

    #[test]
    fn a_damaged_slot_another_writer_locks_stays_published_and_says_what_to_remove() {
        let d = scratch("aside_busy");
        let s = slot_paths(&d, "S");
        std::fs::write(&s.pk, [0x02, 0x01]).unwrap();
        std::fs::write(&s.meta, OLD_META).unwrap();
        let writer = try_lock_cache(&d).unwrap().expect("free");
        let err = read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| multiply_params_for_test())
            .err()
            .expect("damaged");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("remove") && msg.contains("S.meta.json"),
            "{msg}"
        );
        assert!(s.meta.exists(), "a writer is at work: nothing is renamed");
        // The process that holds the lock itself sets it aside.
        read_slot::<BaseCircuitBuilder<Fr>>(&s, true, |_| multiply_params_for_test())
            .err()
            .expect("damaged");
        assert!(!s.meta.exists(), "set aside under its own lock");
        drop(writer);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_unreadable_key_is_not_set_aside() {
        // A permission is not damage: the slot stays published.
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("aside_eacces");
        let s = slot_paths(&d, "S");
        std::fs::write(&s.pk, b"key").unwrap();
        std::fs::write(&s.meta, OLD_META).unwrap();
        std::fs::set_permissions(&s.pk, std::fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&s.pk).is_ok() {
            std::fs::remove_dir_all(&d).ok();
            return;
        }
        let params = multiply_params_for_test();
        assert!(
            read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params).is_err(),
            "EACCES"
        );
        assert!(s.meta.exists(), "still published");
        std::fs::set_permissions(&s.pk, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    /// The params of [`small_key`], without making one.
    fn multiply_params_for_test() -> BaseCircuitParams {
        crate::multiply::multiply_params(8, 7)
    }

    #[test]
    fn the_length_is_added_with_the_metas_own_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("mode");
        let s = slot_paths(&d, "S");
        let (pk, params) = small_key();
        write_key(&s.pk, &pk).unwrap();
        std::fs::write(&s.meta, OLD_META).unwrap();
        // Not a mode any umask leaves a new file with.
        std::fs::set_permissions(&s.meta, std::fs::Permissions::from_mode(0o604)).unwrap();
        read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params).unwrap();
        let md = std::fs::metadata(&s.meta).unwrap();
        assert!(read_meta(&s.meta).unwrap().pk_bytes.is_some());
        assert_eq!(md.permissions().mode() & 0o777, 0o604);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_published_slot_that_fails_to_load_is_an_error_not_a_keygen() {
        // A keygen here would write a second ~6.85 GB key beside the one
        // that failed, and hide why it failed.
        let d = scratch("load_err");
        let s = slot_paths(&d, "S");
        publish_by_hand(&s, b"key");
        let err = load_or_generate::<()>(
            &d,
            &s,
            |_, _| anyhow::bail!("cached aggregator key is damaged"),
            |_| panic!("a published slot is not generated again"),
        )
        .expect_err("the load's error is the caller's");
        assert!(format!("{err:#}").contains("damaged"), "{err:#}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// The meta an older `aggregate-proof` wrote: no key length.
    const OLD_META: &[u8] = br#"{"break_points":[[1]],"calculated":{"degree":8,"num_advice":1,"num_lookup_advice":0,"num_fixed":1,"lookup_bits":7},"num_instance":[1]}"#;

    #[test]
    fn a_slot_published_without_a_length_gets_one_once_its_key_reads_back() {
        // The next preflight can check it then; until then it can only
        // trust it.
        let d = scratch("old_meta");
        let s = slot_paths(&d, "S");
        let (pk, params) = small_key();
        let len = write_key(&s.pk, &pk).unwrap();
        std::fs::write(&s.meta, OLD_META).unwrap();
        let (_, meta) = read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params.clone()).unwrap();
        assert_eq!(meta.pk_bytes, Some(len));
        assert_eq!(
            read_meta(&s.meta).unwrap().pk_bytes,
            Some(len),
            "and it is published"
        );
        assert!(
            sweep_partials(&d).is_empty(),
            "through a partial file of its own"
        );

        // With a writer at work nothing is written: only a holder of the
        // lock writes the cache.
        std::fs::write(&s.meta, OLD_META).unwrap();
        let writer = try_lock_cache(&d).unwrap().expect("free");
        read_slot::<BaseCircuitBuilder<Fr>>(&s, false, |_| params).unwrap();
        assert_eq!(read_meta(&s.meta).unwrap().pk_bytes, None);
        drop(writer);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_generated_keys_meta_records_its_length() {
        let d = scratch("generated");
        let (pk, _) = small_key();
        let k = CachedKeygen {
            pk,
            break_points: vec![vec![1]],
            calculated: AggregationConfigParams::default(),
            num_instance: vec![1],
            slot: None,
        };
        let path = d.join("S.pk.partial-1-0a1b2c3d");
        let meta: CachedMeta = serde_json::from_str(&write_generated(&path, &k).unwrap()).unwrap();
        assert_eq!(meta.pk_bytes, Some(write_key_len(&path)));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_key_this_user_cannot_read_is_not_called_damaged() {
        // Removing it would throw a good key away and cost a keygen.
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("eacces");
        let path = d.join("S.pk");
        std::fs::write(&path, b"key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything: nothing to learn here then.
        if File::open(&path).is_ok() {
            std::fs::remove_dir_all(&d).ok();
            return;
        }
        let params = AggregationConfigParams {
            degree: 4,
            lookup_bits: 3,
            ..Default::default()
        };
        let err = read_slot_key::<AggregationCircuit>(&path, params, None).expect_err("EACCES");
        let msg = format!("{err:#}");
        assert!(msg.contains("S.pk") && !msg.contains("remove"), "{msg}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_damaged_meta_is_called_damaged() {
        let d = scratch("meta");
        let path = d.join("S.meta.json");
        std::fs::write(&path, b"{\"break_points\": [[1]], \"calcul").unwrap();
        let err = read_meta(&path).err().expect("cut short");
        assert!(err.downcast_ref::<DamagedSlot>().is_some(), "{err:#}");
        assert!(
            format!("{err:#}").contains("S.meta.json is damaged"),
            "{err:#}"
        );
        // Bytes that are not text are damaged the same way.
        std::fs::write(&path, [0xff, 0xfe, b'{']).unwrap();
        let err = read_meta(&path).err().expect("not UTF-8");
        assert!(err.downcast_ref::<DamagedSlot>().is_some(), "{err:#}");
        // One an older aggregate-proof wrote has no length, and is read.
        std::fs::write(
            &path,
            br#"{"break_points":[[1]],"calculated":{"degree":21,"num_advice":1,"num_lookup_advice":1,"num_fixed":1,"lookup_bits":20},"num_instance":[24]}"#,
        )
        .unwrap();
        assert_eq!(read_meta(&path).unwrap().pk_bytes, None);
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
        // Keys are read in a format that checks what it reads: the
        // unchecked one panics on a key cut short.
        assert_eq!(
            code.iter()
                .filter(|l| l.contains("RawBytesUnchecked"))
                .count(),
            0,
            "no unchecked key format"
        );
        // A rename is durable once its directory is.
        let publish = &src[src.find("fn publish(").unwrap()..];
        let publish = &publish[..publish.find("\n}\n").unwrap()];
        assert!(publish.contains("sync_dir("), "{publish}");
    }
}
