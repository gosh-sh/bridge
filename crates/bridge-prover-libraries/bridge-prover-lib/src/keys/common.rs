//! Shared VK/PK/config disk I/O helpers used by every per-circuit manager
//! under `crate::keys`. All artefacts live under a shared `params_dir`
//! (default `<crate>/params`) and share a filename convention:
//!
//! - `{prefix}_vk.bin`
//! - `{prefix}_pk.bin`
//! - `{prefix}_config_params.json`
//!
//! Serde format is `SerdeFormat::RawBytesUnchecked` — matches the pre-split
//! monolithic `KeyManager`, so cached artefacts on disk continue to load
//! after the refactor with no migration.

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::{
        bn256::{Bn256, Fr, G1Affine},
        serde::SerdeObject,
    },
    plonk::{ProvingKey, VerifyingKey},
    poly::{commitment::Params, kzg::commitment::ParamsKZG},
    SerdeFormat,
};
use tracing::{info, warn};

pub(crate) const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;

pub(crate) fn vk_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_vk.bin", prefix))
}

pub(crate) fn pk_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_pk.bin", prefix))
}

pub(crate) fn config_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_config_params.json", prefix))
}

pub(crate) fn load_config(
    params_dir: &Path,
    prefix: &str,
) -> anyhow::Result<BaseCircuitParams> {
    let data = std::fs::read_to_string(config_path(params_dir, prefix))?;
    Ok(serde_json::from_str(&data)?)
}

pub(crate) fn save_config(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(config)?;
    write_atomic(&config_path(params_dir, prefix), None, |w| {
        w.write_all(json.as_bytes())?;
        Ok(())
    })
}

pub(crate) fn try_load_vk(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> Option<VerifyingKey<G1Affine>> {
    // The reader `unwrap()`s on a short or malformed file
    // (`halo2curves/src/derive/field.rs:350` under `RawBytesUnchecked`),
    // so without this the `Option` in this signature is decorative.
    //
    // `catch_unwind` has caveats worth stating: it is inert under
    // `panic = "abort"` (not set in this workspace) and the panic message
    // still reaches stderr unless a hook silences it. Neither is a reason
    // to keep aborting the process — `KeyManagerState::new` calls this
    // unconditionally, so a truncated `event_vk.bin` would otherwise take
    // down every manager on this `params_dir`, not just the event one.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = std::fs::File::open(vk_path(params_dir, prefix)).ok()?;
        let mut reader = BufReader::new(file);
        VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }))
    .unwrap_or(None)
}

pub(crate) fn try_load_pk(
    params_dir: &Path,
    prefix: &str,
    config: &BaseCircuitParams,
) -> Option<ProvingKey<G1Affine>> {
    // Same reasoning as `try_load_vk`: `Polynomial::read` is
    // `read_exact(..).unwrap()` plus `F::read(..).unwrap()`
    // (`halo2-axiom/src/poly.rs:169-176`), so a truncated proving key
    // aborts the process instead of returning `None`.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = std::fs::File::open(pk_path(params_dir, prefix)).ok()?;
        let mut reader = BufReader::new(file);
        ProvingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }))
    .unwrap_or(None)
}

pub(crate) fn save_vk(
    params_dir: &Path,
    prefix: &str,
    vk: &VerifyingKey<G1Affine>,
) -> anyhow::Result<()> {
    write_atomic(&vk_path(params_dir, prefix), None, |w| {
        vk.write(w, SERDE_FMT)?;
        Ok(())
    })
}

pub(crate) fn save_pk(
    params_dir: &Path,
    prefix: &str,
    pk: &ProvingKey<G1Affine>,
) -> anyhow::Result<()> {
    write_atomic(&pk_path(params_dir, prefix), None, |w| {
        pk.write(w, SERDE_FMT)?;
        Ok(())
    })
}

// -- Key manifest ------------------------------------------------------------

/// `{prefix}_manifest.json`, written next to the keys by `run_keygen`.
///
/// Digests rather than lengths. A length catches truncation and nothing
/// else, and `SerdeFormat::RawBytesUnchecked` means a same-length byte flip
/// deserialises without complaint — so the file would load,
/// `transcript_repr()` would still match (it digests the VK, not the PK),
/// and the cache would read as warm while the proof it produces is invalid.
///
/// Verifying by digest first is also what makes the probe panic-free: both
/// halo2 readers `unwrap()` on malformed input, so nothing may reach them
/// until its bytes have been checked.
///
/// # Two guards against a future shape, because they catch different changes
///
/// Neither is redundant, and having only one is how "a future format is
/// rejected" becomes a comment that is not true:
///
/// - `deny_unknown_fields` catches a future manifest that **adds** a field.
///   Without it serde silently ignores unknown fields, so a build that predates
///   the addition reads the fields it recognises, decides the cache is warm,
///   and never learns that a field it does not know about was qualifying them.
/// - `manifest_format` catches a future manifest that **reuses** the existing
///   fields differently — digests switched to another algorithm,
///   `circuit_revision` given a namespace. Nothing about the JSON changes shape
///   there, so `deny_unknown_fields` sees nothing wrong; only an explicit
///   version does.
///
/// The version field is also the tree's established pattern rather than a
/// new invention: the aggregator's cache slots carry a `v2` format tag for
/// exactly this reason (`bridge-evm-aggregator/src/aggregator_cache.rs:11-29`).
///
/// It is first in the struct so that it is first in the serialised JSON,
/// which makes a manifest legible to a human running `cat` before it is
/// legible to serde.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyManifest {
    pub manifest_format: u32,
    pub circuit_revision: u32,
    pub pk_sha256: String,
    pub vk_sha256: String,
    pub config_sha256: String,
}

/// On-disk shape of [`KeyManifest`].
///
/// Bump when a field changes MEANING. Do not bump merely to add one:
/// `deny_unknown_fields` already makes every older build reject a manifest
/// with a field it does not know, which is the fail-closed behaviour a bump
/// would be trying to buy.
///
/// Distinct from `EVENT_CIRCUIT_REVISION`, and the two are easy to
/// conflate. The revision describes the CIRCUIT the keys were built for;
/// this describes the FILE that says so. A circuit change bumps the first
/// and leaves this alone.
pub(crate) const MANIFEST_FORMAT: u32 = 1;

pub(crate) fn manifest_path(params_dir: &Path, prefix: &str) -> PathBuf {
    params_dir.join(format!("{}_manifest.json", prefix))
}

/// Read the manifest, distinguishing "there is none" from "there is one and
/// it is unusable".
///
/// `Ok(None)` is a cache written before this mechanism existed: expected,
/// benign, regenerate quietly. `Err` is a manifest that is *there* and
/// cannot be trusted to say anything — truncated, hand-edited, unreadable,
/// or in a format this build does not know. Same remedy, different message,
/// and only one of them deserves a `warn!`.
///
/// Both callers need that split. `probe_event_key_cache` prints one of two
/// refusal texts; `KeyManagerState::new` collapses them again, but only
/// after deciding to, which is not the same as never having had the choice.
///
/// EACCES is the case that makes an `Option` actively misleading:
/// `read_to_string` fails on an unreadable manifest exactly as it does on a
/// missing one, so folding them reports a permissions problem as "this
/// cache predates the check" and sends the operator to regenerate keys that
/// were fine.
pub(crate) fn read_manifest(
    params_dir: &Path,
    prefix: &str,
) -> anyhow::Result<Option<KeyManifest>> {
    let path = manifest_path(params_dir, prefix);
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        // The ONLY error kind that means "absent". Everything else — a
        // directory in its place, EACCES, EIO — is a manifest this build
        // cannot read, not a manifest that was never written.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("read {}", path.display()));
        },
    };
    // A parse failure is an `Err`, never `Ok(None)`: every one of these is a
    // manifest that EXISTS, and only a missing file is `Ok(None)`.
    //
    // What serde rejects here: a truncated file, a stray `null`, a missing
    // `manifest_format`, and — because of `deny_unknown_fields` on the
    // struct — a manifest carrying a field this build does not know. A
    // future format that reuses these same fields with a different meaning
    // parses cleanly and is caught one step later, by the `manifest_format`
    // comparison.
    let m: KeyManifest =
        serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(m))
}

/// Hash the three files now on disk and record them, with the circuit
/// revision, beside the keys.
///
/// Hashes what it finds rather than what the caller holds in memory: the
/// digests exist to describe the bytes a later run will read, and the
/// in-memory key is not those bytes until it has been written and synced.
/// This is why `run_keygen` calls it strictly after all three `save_*`
/// calls have returned.
pub(crate) fn save_manifest(
    params_dir: &Path,
    prefix: &str,
    circuit_revision: u32,
) -> anyhow::Result<()> {
    let m = KeyManifest {
        manifest_format: MANIFEST_FORMAT,
        circuit_revision,
        pk_sha256: sha256_file(&pk_path(params_dir, prefix))?,
        vk_sha256: sha256_file(&vk_path(params_dir, prefix))?,
        config_sha256: sha256_file(&config_path(params_dir, prefix))?,
    };
    let json = serde_json::to_string_pretty(&m)?;

    // The mode of the config this manifest describes, as the fallback for a
    // manifest that is not there. It usually is not: `run_keygen` retracts
    // the manifest before writing any key, so on every regeneration
    // `write_atomic` finds nothing to copy a mode from and would fall back
    // to `0666 & ~umask` — `0600` under umask 077, which locks a separate
    // `bridge-verifier-daemon` out of the one file it must read.
    //
    // `.ok()`: the config was written moments ago by this same `run_keygen`,
    // so a failure to stat it means something stranger is happening than a
    // permissions question, and the umask default is a fine answer to it.
    let mode_hint = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(config_path(params_dir, prefix))
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    };

    // Through `write_atomic` like the three key savers. A half-written
    // manifest is the one input `read_manifest` cannot classify usefully —
    // it is neither absent nor trustworthy — so the writer's job is to make
    // that state unreachable.
    write_atomic(&manifest_path(params_dir, prefix), mode_hint, |w| {
        w.write_all(json.as_bytes())?;
        Ok(())
    })
}

/// Streaming SHA-256 of a file, hex-encoded.
///
/// Streaming, not `fs::read`: the proving key is ~2.65 GB and reading it
/// into memory to hash it would cost more than the check saves.
///
/// `pub(super)` because `probe_event_key_cache` lives in `keys::state`.
pub(super) fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};

    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Write `path` atomically: a temp sibling, synced, then renamed over the
/// destination.
///
/// A partial file here is worse than a missing one. Missing means cold,
/// which regenerates; partial means "cached" to `keys_cached()` and fails
/// at load time, every time, until someone deletes it by hand.
///
/// # Permissions
///
/// **Reproduces `File::create`, deliberately, because the switch to a temp
/// file silently changes them otherwise.** `tempfile` creates with mode
/// `0600` (`tempfile-3.27.0/src/file/imp/unix.rs:23`) and `persist` is a
/// rename, so the new mode travels with the file. `File::create` opens with
/// `0o666`, which the kernel masks with the umask, and leaves an EXISTING
/// file's mode untouched.
///
/// Measured on this tree, at umask 022 / 077 / 002:
///
/// | how | new file | over an existing 0644 | manifest after retract+save |
/// |---|---|---|---|
/// | `File::create` (before) | umask | 0644 | n/a |
/// | `NamedTempFile::new_in` + persist | umask | **0600** | **0600** |
/// | this function, `mode_hint: None` | umask | 0644 | **0600 at umask 077** |
/// | this function, with the hint | umask | 0644 | 0644 |
///
/// The last column is why `mode_hint` exists. `run_keygen` REMOVES the
/// manifest before it writes any key (`retract_manifest`), so by the time
/// `save_manifest` runs there is no existing file to copy a mode from and
/// the "preserve what is there" rule has nothing to preserve. It lands at
/// `0666 & ~umask`, which under umask 077 is `0600` — and the manifest is
/// exactly the file a *second* process must read: `KeyManagerState::new`
/// calls `read_manifest`, so a `bridge-verifier-daemon` under another
/// account gets `EACCES`, no manifest, no installed VK, and the message
/// "event VK not found" about a file that is right there.
///
/// So the manifest's saver passes the mode of the config beside it. Same
/// directory, same audience, same lifecycle, and it is written immediately
/// before — "a manifest is as readable as the artifacts it describes".
///
/// # Ownership is NOT preserved, and cannot be
///
/// `rename` publishes a NEW inode, owned by whoever wrote it. `File::create`
/// on an existing file keeps that file's uid/gid because it never makes a
/// new one. So if an artifact in `params/` is owned by another account —
/// seeded by a provisioning script, or by root — the first regeneration
/// moves it to the account running keygen. Non-root cannot chown it back,
/// so no implementation of atomic-rename can avoid this; the only choice is
/// whether to say so. (Mode still survives, which is what governs whether
/// the daemon can read it.)
///
/// This function serves all four savers and therefore all four managers —
/// primary, fallback, layer and event — so the naive version would quietly
/// re-mode every cached verifying key and config on the next keygen. On the
/// shipped profile `params/` is shared ("shared with the bundle daemon"),
/// and a bundle daemon running as its own service account would lose read
/// access to files it had yesterday, at the next keygen, with no message.
///
/// Nothing here is secret — a verifying key is published and an SRS is a
/// public ceremony — so tightening to `0600` would buy nothing and cost
/// that.
///
/// Do not "unify" this with the other atomic write in the tree. The
/// idempotency record in `ackinacki-bridge` uses `NamedTempFile::new_in`
/// precisely FOR the `0600`, and asserts it; that file holds a withdrawal's
/// state and lives under the operator's state dir. The two want opposite
/// things, which is why they are two functions.
fn write_atomic(
    path: &Path,
    // Mode to use when `path` does not exist yet. `None` means "whatever
    // `File::create` would produce here", i.e. `0o666` masked by the umask.
    // Only `save_manifest` passes `Some`, because only the manifest is
    // deleted and recreated within one keygen.
    mode_hint: Option<u32>,
    write: impl FnOnce(&mut BufWriter<&File>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    use std::{fs::Permissions, os::unix::fs::PermissionsExt};

    let dir = path.parent().context("key path has no parent")?;
    // Existing file wins over the hint: a mode an operator actually set is
    // better evidence than a sibling's.
    let target = std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
        .or(mode_hint);

    // `Builder::permissions` feeds `OpenOptions::mode`, which the KERNEL
    // masks with the umask. That is exactly right when we have no target
    // mode — it is what `File::create`'s `0o666` does — and wrong when we
    // do, because `File::create` would not have touched the mode at all.
    let tmp = tempfile::Builder::new()
        .permissions(Permissions::from_mode(target.unwrap_or(0o666)))
        .tempfile_in(dir)?;

    // So chmod that case back. `set_permissions` is not umask-filtered,
    // unlike the mode passed to `open`. Skipping this looks harmless under
    // the usual umask 022 (0644 & ~022 is still 0644) and silently tightens
    // a shared 0644 key to 0600 under umask 077 — a bug that would only ever
    // appear on someone else's host.
    if let Some(mode) = target {
        tmp.as_file()
            .set_permissions(Permissions::from_mode(mode))?;
    }
    {
        let mut w = BufWriter::new(tmp.as_file());
        write(&mut w)?;
        w.flush()?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    // Propagated, NOT discarded. The rename is not durable until the
    // directory entry is, and the whole "manifest present implies the set is
    // whole" argument rests on the three key files surviving a crash that
    // the manifest also survives. A swallowed error here would let the
    // manifest land while one of the keys did not — leaving `Corrupt` where
    // the design promises a self-healing `Cold`.
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| {
            format!(
                "fsync {} after publishing {}",
                dir.display(),
                path.display()
            )
        })?;
    Ok(())
}

/// Load a KZG SRS whose `params.k()` is exactly `k`.
/// Resolution order:
/// 1. Fast path: `kzg_bn254_{k}.srs` exists and its header matches `k` →
///    verify Hermez provenance and return.
/// 2. Otherwise scan `params_dir` for the largest `kzg_bn254_*.srs` with
///    header degree ≥ `k`, downsize, and write to the exact path so the
///    next load hits the fast path.
pub(crate) fn load_srs(params_dir: &Path, k: u32) -> ParamsKZG<Bn256> {
    let exact_path = params_dir.join(format!("kzg_bn254_{k}.srs"));

    // Same resolution the preflight probe performs — literally the same
    // function, so the two cannot drift. `load_srs` panics where
    // `probe_ceremony` returns an error; that is the only difference.
    if let Ok((src_path, mut srs)) = resolve_ceremony(params_dir, k) {
        // The fast path: the exact file was already there and already the
        // right degree. Nothing to downsize, nothing to persist.
        if src_path == exact_path && srs.k() == k {
            return srs;
        }
        let src_k = srs.k();
        if src_k > k {
            srs.downsize(k);
        }
        info!(
            target: "bridge_prover_lib::keys",
            src = %src_path.display(),
            src_k,
            circuit_k = k,
            dest = %exact_path.display(),
            "provisioned circuit SRS by downsizing parent Hermez ceremony"
        );
        if let Err(e) = write_srs_file(&exact_path, &srs) {
            warn!(
                target: "bridge_prover_lib::keys",
                path = %exact_path.display(),
                error = %e,
                "failed to persist downsized SRS; continuing with in-memory params"
            );
        }
        return srs;
    }

    panic!(
        "no Hermez Perpetual Powers of Tau SRS (>= k={k}) under {}. \
         Provision it with the `bootstrap_hermez_srs` bin in this crate:\n\
         \x20 cargo build --release --bin bootstrap_hermez_srs\n\
         \x20 ./target/release/bootstrap_hermez_srs --k 21 --params-dir <this dir>\n\
         K=21 additionally needs powersOfTau28_hez_final_21.ptau (~2.4 GB) at \
         ~/.cache/halo2-kzg-srs/ — it is NOT auto-downloaded; fetch it from \
         https://storage.googleapis.com/zkevm/ptau/. \
         NOTE: scripts/bootstrap_hermez_srs.sh is a different tool — it writes \
         K=20 into crates/bridge-snark-utils/params/ and will not satisfy this. \
         Chain-ceremony / gen_srs fallbacks are disabled.",
        params_dir.display()
    );
}

/// The resolution [`load_srs`] performs, with the parsed params kept.
///
/// Private: the single place the rules live, so the preflight and the
/// runtime cannot drift. Exact path first — `load_srs` prefers
/// `kzg_bn254_{k}.srs` by name over any larger ceremony, which is why a
/// bad file at the exact degree is not rescued by a good larger one — then
/// the largest `kzg_bn254_*.srs` of degree >= `min_k`. Provenance is
/// asserted on whichever wins.
///
/// Returns the params rather than just the degree so that the one
/// deserialisation serves both callers.
fn resolve_ceremony(
    params_dir: &Path,
    min_k: u32,
) -> anyhow::Result<(PathBuf, ParamsKZG<Bn256>)> {
    let exact = params_dir.join(format!("kzg_bn254_{min_k}.srs"));
    if let Ok(srs) = read_srs_file(&exact) {
        if srs.k() == min_k {
            assert_hermez_srs(&srs).with_context(|| format!("{}", exact.display()))?;
            return Ok((exact, srs));
        }
    }
    let (path, srs) = find_largest_ceremony_ge(params_dir, min_k).ok_or_else(|| {
        anyhow::anyhow!(
            "no Hermez ceremony of degree >= {min_k} under {}",
            params_dir.display()
        )
    })?;
    assert_hermez_srs(&srs).with_context(|| format!("{}", path.display()))?;
    Ok((path, srs))
}

/// Resolve which ceremony file [`load_srs`] would use for `min_k`, without
/// keeping the parsed params.
///
/// Same scan, same provenance rules, same failure conditions as
/// [`load_srs`] — including the full deserialization, which is the only
/// way to know a file is usable rather than merely present. Exposed so a
/// caller can fail fast at its own preflight instead of discovering the
/// problem when it first proves. Callers MUST NOT approximate this with a
/// filename or header sniff: a truncated file passes those and fails here.
pub fn probe_ceremony(params_dir: &Path, min_k: u32) -> anyhow::Result<(PathBuf, u32)> {
    let (path, srs) = resolve_ceremony(params_dir, min_k)?;
    // The params are dropped here, and that is the whole difference
    // between this and `load_srs`. The preflight wants the verdict, not
    // ~256 MB held across the rest of stage 1.
    Ok((path, srs.k()))
}

/// Hermez `s_g2` head (`928fafb3d0cc…`). Rejects any untrusted chain ceremony
/// any synthetic `gen_srs` trapdoor.
pub const HERMEZ_S_G2_HEAD: [u8; 6] = [0x92, 0x8f, 0xaf, 0xb3, 0xd0, 0xcc];

/// Refuse to proceed unless `srs` was produced by the Hermez Perpetual
/// Powers of Tau ceremony (identified by its `s_g2` head, see
/// [`HERMEZ_S_G2_HEAD`]). Called both from [`load_srs`] (via
/// [`resolve_ceremony`], which every ceremony lookup now goes through) and
/// from the offline snark exporters in
/// `bridge-snark-utils` which call `gen_srs` directly — no toxic-waste
/// SRS proceeds past this check.
pub fn assert_hermez_srs(srs: &ParamsKZG<Bn256>) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(128);
    srs.s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    if buf.len() < HERMEZ_S_G2_HEAD.len() {
        anyhow::bail!(
            "SRS s_g2 encoding too small ({} bytes) — SRS at k={} is malformed",
            buf.len(),
            srs.k(),
        );
    }
    let head = &buf[..HERMEZ_S_G2_HEAD.len()];
    if head != HERMEZ_S_G2_HEAD {
        anyhow::bail!(
            "SRS (k={}) is NOT Hermez Perpetual Powers of Tau \
             (s_g2 head {:02x?}, expected {:02x?}). PARAMS_DIR likely lacks \
             kzg_bn254_{}.srs and `gen_srs` silently generated a toxic-waste \
             SRS whose tau is known to the local process — every proof \
             produced with it is forgeable. Bootstrap via \
             `bootstrap_hermez_srs`. REFUSING to proceed.",
            srs.k(),
            head,
            HERMEZ_S_G2_HEAD,
            srs.k(),
        );
    }
    Ok(())
}

fn read_srs_file(path: &Path) -> std::io::Result<ParamsKZG<Bn256>> {
    let file = std::fs::File::open(path)?;
    // `ParamsKZG::read` PANICS on a short file rather than returning its
    // `io::Result`: the per-element reader is `read_exact(..).unwrap()`
    // (`halo2curves-axiom/src/bn256/fq.rs:132`), so the header can parse and
    // the point loop then run out of input.
    //
    // Same treatment as `try_load_vk` / `try_load_pk`, and needed for the
    // same reason twice over. `probe_ceremony` exists to REPORT that a
    // ceremony file is unusable — a truncated one whose 128-byte tail
    // happens to open with the Hermez head reaches the loop and takes the
    // process down, so the preflight refusal this whole check exists to
    // print never gets printed. And `find_largest_ceremony_ge` calls this in
    // a scan, where one bad file among several must be skipped past rather
    // than abort the run.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let mut reader = BufReader::new(file);
        ParamsKZG::<Bn256>::read(&mut reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }))
    .unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{}: SRS is truncated or malformed (the halo2 reader ran out of input)",
                path.display()
            ),
        ))
    })
}

fn write_srs_file(path: &Path, srs: &ParamsKZG<Bn256>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    srs.write(&mut writer)?;
    writer.flush()?;
    Ok(())
}

/// Scan `params_dir` for `kzg_bn254_{N}.srs` files whose header degree is
/// ≥ `min_k`, returning the largest such ceremony.
fn find_largest_ceremony_ge(
    params_dir: &Path,
    min_k: u32,
) -> Option<(PathBuf, ParamsKZG<Bn256>)> {
    let entries = std::fs::read_dir(params_dir).ok()?;
    let mut best: Option<(u32, PathBuf, ParamsKZG<Bn256>)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let Some(k_str) = name
            .strip_prefix("kzg_bn254_")
            .and_then(|s| s.strip_suffix(".srs"))
        else {
            continue;
        };
        // Filename hint only — trust the header after read.
        if k_str.parse::<u32>().is_err() {
            continue;
        }
        let srs = match read_srs_file(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if srs.k() < min_k {
            continue;
        }
        // Skip non-Hermez ceremonies (e.g. leftover chain-ceremony files).
        //
        // Against the PARSED object, not the file's tail. `assert_hermez_srs`
        // encodes `srs.s_g2()` into a 128-byte buffer and compares the same
        // six bytes, so this is the identical test — and it is what
        // `load_srs`'s exact-path branch already does. The byte-offset
        // version was the only place in the file reaching around the parser.
        //
        // What goes away: a second full read of a ~256 MB file per
        // candidate, and a raw `Vec<u8>` of that size held alongside the
        // parsed `ParamsKZG` it duplicates.
        //
        // `is_err()` and `continue`, not `?`: this is a scan, and a foreign
        // ceremony in the directory is something to skip past rather than
        // an error. That is today's behaviour and it stays.
        if assert_hermez_srs(&srs).is_err() {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((best_k, _, _)) => srs.k() > *best_k,
        };
        if replace {
            best = Some((srs.k(), path, srs));
        }
    }
    best.map(|(_, path, srs)| (path, srs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Smallest Hermez ceremony actually kept under
    /// `crates/bridge-prover-libraries/params/` (see repo layout). Larger than
    /// strictly necessary for these tests, but avoids committing a
    /// dedicated fixture ceremony just for the unit suite.
    const FIXTURE_SRC_K: u32 = 17;
    /// Downsize target — arbitrary as long as `< FIXTURE_SRC_K`.
    const FIXTURE_DST_K: u32 = 15;

    fn fixture_srs(k: u32) -> PathBuf {
        // Repo layout: workspace member lives at
        // `crates/bridge-prover-libraries/bridge-prover-lib`; the params/ dir
        // lives at `crates/bridge-prover-libraries/params`. Cover that first, then
        // fall back to legacy repo-root / CWD paths.
        let candidates = [
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../params"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../params"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../params"),
            PathBuf::from("params"),
        ];
        for dir in candidates {
            let p = dir.join(format!("kzg_bn254_{k}.srs"));
            if p.exists() {
                return p;
            }
        }
        panic!("missing fixture params/kzg_bn254_{k}.srs — run from repo with params/");
    }

    #[test]
    fn load_srs_downsizes_from_parent_ceremony_when_exact_missing() {
        let src = fixture_srs(FIXTURE_SRC_K);
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::copy(&src, tmp.path().join(format!("kzg_bn254_{FIXTURE_SRC_K}.srs"))).unwrap();

        let srs = load_srs(tmp.path(), FIXTURE_DST_K);
        assert_eq!(srs.k(), FIXTURE_DST_K);
        assert!(tmp.path().join(format!("kzg_bn254_{FIXTURE_DST_K}.srs")).exists());

        // Same toxic waste as the parent ceremony.
        let parent = read_srs_file(&src).unwrap();
        assert_eq!(format!("{:?}", srs.s_g2()), format!("{:?}", parent.s_g2()));
    }

    #[test]
    fn load_srs_downsizes_misnamed_larger_file() {
        let src = fixture_srs(FIXTURE_SRC_K);
        let tmp = tempfile::tempdir().expect("tempdir");
        // Partner-style footgun: FIXTURE_SRC_K bytes living under the
        // FIXTURE_DST_K filename.
        std::fs::copy(&src, tmp.path().join(format!("kzg_bn254_{FIXTURE_DST_K}.srs"))).unwrap();

        let srs = load_srs(tmp.path(), FIXTURE_DST_K);
        assert_eq!(srs.k(), FIXTURE_DST_K);
        let reread =
            read_srs_file(&tmp.path().join(format!("kzg_bn254_{FIXTURE_DST_K}.srs"))).unwrap();
        assert_eq!(reread.k(), FIXTURE_DST_K);
    }

    // -- ceremony probe ---------------------------------------------------

    #[test]
    fn probe_rejects_a_truncated_file_that_has_a_plausible_tail() {
        use std::io::Write;
        // 256 bytes whose last 128 open with the Hermez s_g2 head: passes any
        // filename+tail sniff, contains no usable points. This is the case a
        // hand-rolled preflight check would wave through.
        let tmp = tempfile::TempDir::new().unwrap();
        let mut bytes = vec![0u8; 256];
        let off = bytes.len() - 128;
        bytes[off..off + 6].copy_from_slice(&HERMEZ_S_G2_HEAD);
        let mut f = std::fs::File::create(tmp.path().join("kzg_bn254_21.srs")).unwrap();
        f.write_all(&bytes).unwrap();

        assert!(
            probe_ceremony(tmp.path(), 21).is_err(),
            "a file load_srs cannot deserialize must not probe as usable",
        );
    }

    #[test]
    fn probe_rejects_an_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(probe_ceremony(tmp.path(), 21).is_err());
    }

    /// The repo's `params/`, or a hard failure saying why it is not there.
    ///
    /// An `assert`, not an early `return`. libtest counts a
    /// skipped-by-return test as a PASS — so on every runner without the
    /// ceremony, the two tests that prove the ceremony check works would
    /// report green while checking nothing.
    ///
    /// They carry `#[ignore]` instead, so a host without `params/` does not
    /// run them at all and says so, while the scheduled `keycache` job —
    /// which symlinks its provisioned ceremony into this exact path before
    /// running `keys:: -- --include-ignored` — does run them, and a missing
    /// file there is a broken job rather than a silent pass.
    fn repo_params_dir() -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../params");
        assert!(
            dir.join("kzg_bn254_21.srs").is_file(),
            "{} has no kzg_bn254_21.srs. This test is #[ignore]d precisely so it only runs where \
             the ceremony exists; provision it with `cargo run --release -p bridge-prover-lib \
             --bin bootstrap_hermez_srs`, or drop --include-ignored.",
            dir.display(),
        );
        dir
    }

    /// A well-formed SRS at degree `k` that is NOT the Hermez ceremony.
    ///
    /// `ParamsKZG::setup` draws its own toxic waste and touches nothing on
    /// disk — which is the entire reason it is used here rather than
    /// `halo2_base::utils::fs::gen_srs`.
    ///
    /// `gen_srs` is a trap for this test. It is `read_or_create_srs`
    /// underneath: it reads `$PARAMS_DIR/kzg_bn254_{k}.srs`, or
    /// `./params/kzg_bn254_{k}.srs`, **and creates that file when it is
    /// missing**. Both branches are wrong here, and both are silent:
    ///
    /// - the file exists → the "decoy" IS the real Hermez ceremony, the probe
    ///   accepts it, and the test fails claiming the check is broken;
    /// - the file is missing → a toxic-waste SRS is written into the operator's
    ///   (or CI's persistent) `params/`, where a later production run will load
    ///   it and panic.
    ///
    /// A unit test must not be able to poison a ceremony cache.
    fn write_non_hermez_srs(path: &Path, k: u32) {
        use rand::rngs::OsRng;

        let srs = ParamsKZG::<Bn256>::setup(k, OsRng);
        write_srs_file(path, &srs).expect("write the decoy ceremony");
    }

    #[test]
    #[ignore = "needs a provisioned params/; runs in test:rust:ackinacki-bridge:keycache"]
    fn probe_accepts_the_repo_ceremony() {
        let dir = repo_params_dir();
        let (path, k) =
            probe_ceremony(&dir, 21).expect("the provisioned ceremony must probe clean");
        assert!(k >= 21, "probe reported k={k}");
        assert!(path.exists());
    }

    #[test]
    #[ignore = "needs a provisioned params/; runs in test:rust:ackinacki-bridge:keycache"]
    fn a_good_k21_does_not_excuse_a_bad_k20() {
        // The regression a k=21-only check misses. `load_srs(dir, 20)` reads
        // `kzg_bn254_20.srs` by exact name before it considers downsizing the
        // k=21 file, and a foreign `s_g2` is refused there. Preflight must
        // reach the same file.
        let dir = repo_params_dir();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::copy(
            dir.join("kzg_bn254_21.srs"),
            tmp.path().join("kzg_bn254_21.srs"),
        )
        .unwrap();
        // A k=20 file with a valid header and a non-Hermez tail.
        let bad = tmp.path().join("kzg_bn254_20.srs");
        write_non_hermez_srs(&bad, 20);

        assert!(
            probe_ceremony(tmp.path(), 21).is_ok(),
            "the k=21 file is good — a k=21 probe passes, which is the trap"
        );
        assert!(
            probe_ceremony(tmp.path(), 20).is_err(),
            "the k=20 file is not Hermez and must be refused"
        );
    }

    // -- atomic writes preserve modes -------------------------------------

    #[test]
    #[cfg(unix)]
    fn saving_a_key_artifact_does_not_change_its_mode() {
        // `tempfile` creates at 0600 and `persist` renames, so the naive
        // atomic write re-modes every artifact it touches — for all four
        // managers, not just the event one. On the shipped profile `params/`
        // is shared with the bundle daemon; if that daemon runs as another
        // account it loses its VK and config at the next keygen, silently,
        // with no message anywhere.
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        let d = tempfile::TempDir::new().unwrap();

        // What `File::create` produces HERE. Not a hard-coded 0644: the
        // umask is the runner's, not this test's, and hard-coding it would
        // make the test fail on a host with umask 002 or 077 for a reason
        // that has nothing to do with the bug.
        let probe = d.path().join("umask_probe");
        std::fs::File::create(&probe).unwrap();
        let expected_new = std::fs::metadata(&probe).unwrap().permissions().mode() & 0o777;

        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        let p = config_path(d.path(), "event");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            expected_new,
            "a new key artifact must land with the mode File::create would have given it, not \
             tempfile's 0600"
        );

        // The case that actually breaks a deployment: an artifact that is
        // already group- or world-readable stays that way. 0644 explicitly,
        // because that is what a params/ shared with another service looks
        // like — and note this must hold under ANY umask, which is why the
        // implementation chmods rather than relying on the mode it passed to
        // `open`.
        std::fs::set_permissions(&p, Permissions::from_mode(0o644)).unwrap();
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o644,
            "rewriting a cached artifact must not change who can read it"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_regenerated_manifest_keeps_the_mode_of_the_keys_it_describes() {
        // The retract/save cycle, which the mode-preservation rule alone
        // does NOT cover: `run_keygen` removes the manifest before writing
        // any key, so `save_manifest` always creates it fresh and has no
        // previous mode to copy. Without the config-mode hint it lands at
        // `0666 & ~umask` — `0600` under umask 077 — and the manifest is the
        // one artifact a SECOND process has to read: `KeyManagerState::new`
        // calls `read_manifest`, so `bridge-verifier-daemon` under another
        // account would get EACCES and report "event VK not found".
        //
        // Simulated at this level rather than through `run_keygen`, which
        // would need an SRS and seven minutes. The removal is the only part
        // of the cycle that matters here.
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        let d = tempfile::TempDir::new().unwrap();
        // `save_manifest` hashes all three; dummy bytes are enough.
        save_config(d.path(), "event", &BaseCircuitParams::default()).unwrap();
        std::fs::write(d.path().join("event_vk.bin"), vec![0u8; 64]).unwrap();
        std::fs::write(d.path().join("event_pk.bin"), vec![0u8; 64]).unwrap();

        // A params/ shared with a daemon running as another account.
        std::fs::set_permissions(
            config_path(d.path(), "event"),
            Permissions::from_mode(0o644),
        )
        .unwrap();

        save_manifest(d.path(), "event", 1).unwrap();
        let m = manifest_path(d.path(), "event");
        std::fs::set_permissions(&m, Permissions::from_mode(0o644)).unwrap();

        // Retract, then republish — the ordering `run_keygen` uses.
        std::fs::remove_file(&m).unwrap();
        save_manifest(d.path(), "event", 1).unwrap();

        assert_eq!(
            std::fs::metadata(&m).unwrap().permissions().mode() & 0o777,
            0o644,
            "a republished manifest must stay as readable as the config beside it; falling back \
             to the umask locks out every reader that is not the writer"
        );
    }
}
