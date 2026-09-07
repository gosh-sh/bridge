//! Report what the Circuit-4 key cache in `--params-dir` is, and
//! optionally clear it when it is unusable.
//!
//! Exists because `Corrupt` is the one cache state nothing self-heals:
//! `keys_cached()` sees a PK file and skips keygen, so the run fails at
//! `load_pk` forever. The refusal text tells an operator which files to
//! delete; this does it for them, and gives CI something to call.
//!
//! `Blocked` it deliberately does not repair: clearing a key cache and
//! deleting a directory somebody mounted are not the same action, and only
//! the first is this tool's remit. That arm refuses before any file is
//! removed, so a run that cannot finish does not start.

use std::path::PathBuf;

use anyhow::{bail, Result};
use bridge_prover_lib::keys::{
    format_bytes, leaked_keygen_temp_files, probe_event_key_cache, KeyCacheState,
};

fn main() -> Result<()> {
    let mut params_dir: Option<PathBuf> = None;
    let mut repair = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--params-dir" => {
                params_dir = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--params-dir needs a path"))?,
                ))
            },
            "--repair" => repair = true,
            "-h" | "--help" => {
                println!("probe_event_keys --params-dir <dir> [--repair]");
                return Ok(());
            },
            other => bail!("unknown argument: {other}"),
        }
    }
    let dir = params_dir.ok_or_else(|| anyhow::anyhow!("--params-dir is required"))?;

    // Before the verdict, and reported whatever the verdict is. A leaked
    // temp is orthogonal to whether the keys are usable — a warm cache can
    // be sitting next to 2.65 GB of litter — and it is invisible to the
    // operator otherwise: the name starts with a dot, so a plain `ls` does
    // not list it, and until now nothing removed it either.
    let leaks = leaked_keygen_temp_files(&dir);
    if !leaks.is_empty() {
        let total: u64 = leaks.iter().map(|l| l.bytes).sum();
        // `format_bytes`, not a fixed GB: the interesting leak is a
        // ~2.65 GiB proving key, but a killed config write leaves a few
        // hundred bytes, and printing "0.00 GB" next to a path reads as
        // "this one is nothing" for a file the operator still has to
        // delete by hand.
        println!(
            "leaked: {} interrupted-keygen temp file(s), {} total",
            leaks.len(),
            format_bytes(total),
        );
        for l in &leaks {
            println!("  {} ({})", l.path.display(), format_bytes(l.bytes));
        }
        if !repair {
            println!("  pass --repair to remove them");
        }
    }

    match probe_event_key_cache(&dir) {
        KeyCacheState::Warm => println!("warm: keys are usable"),
        KeyCacheState::Cold { why } => println!("cold: {why}"),
        // Before `--repair` is even consulted, and before a single file is
        // removed. This arm exists because the alternative — discovering it
        // inside the deletion loop — means the operator has already been
        // told "pass --repair to clear it" by the arm below and has already
        // watched some files disappear before the refusal.
        KeyCacheState::Blocked { why } => {
            println!("blocked: {why}");
            // "nothing was changed" has to stay literally true, so this
            // arm refuses ahead of the temp-file sweep below as well as
            // ahead of the key deletions. Say so, or an operator who was
            // just shown a leak list reads the refusal as "and the temps
            // are gone".
            if !leaks.is_empty() {
                println!(
                    "the {} leaked temp file(s) above were left alone too",
                    leaks.len()
                );
            }
            bail!("this is not something --repair can clear; nothing was changed");
        },
        KeyCacheState::Corrupt { why } => {
            println!("corrupt: {why}");
            if !repair {
                // Non-zero so a caller without --repair notices.
                bail!("pass --repair to clear it");
            }
            // Manifest first, and the order in this array is the order of
            // deletion: if the process dies mid-repair, a missing manifest
            // already reads as cold, so the cache cannot come back as a
            // false hit.
            for f in [
                "event_manifest.json",
                "event_pk.bin",
                "event_vk.bin",
                "event_config_params.json",
            ] {
                match std::fs::remove_file(dir.join(f)) {
                    Ok(()) => {},
                    // Already gone is the goal, not a failure.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                    // Defence in depth. The `Blocked` arm above already
                    // refused this before any deletion, so reaching here
                    // means a directory appeared between the probe and this
                    // removal. Say so rather than reporting a generic errno:
                    // `remove_dir_all` would delete whatever an operator (or
                    // a bind mount) put there, which is a different and much
                    // worse action than clearing a key cache.
                    Err(e) if dir.join(f).is_dir() => bail!(
                        "{} is a directory, not a key file. This tool clears key files; it will \
                         not delete a directory it did not create. Check whether something is \
                         bind-mounted there, then remove it by hand and re-run. ({e})",
                        dir.join(f).display(),
                    ),
                    // Anything else — read-only mount, permissions — must
                    // surface. Swallowing it and printing "repaired" would
                    // send the operator back to a cache that is still
                    // broken, with a message saying it is not.
                    Err(e) => bail!("could not remove {}: {e}", dir.join(f).display()),
                }
            }
            // Confirm rather than assert. The probe is cheap now that the
            // files are gone, and "I deleted some files" is not the same
            // claim as "the cache is now cold".
            match probe_event_key_cache(&dir) {
                KeyCacheState::Cold { .. } => {
                    println!("repaired: cache cleared; the next run regenerates it")
                },
                other => bail!("repair did not take effect: {other:?}"),
            }
        },
    }

    // Sweep the litter LAST, and independently of the cache verdict: a
    // warm cache next to a leaked 2.65 GB temp is the common case, and it
    // never reaches the `Corrupt` arm above.
    //
    // `--repair` is required, like every other deletion here. The paths
    // come from `leaked_keygen_temp_files`, which matches only
    // `.tmp` + six alphanumerics and only regular files, so a directory or
    // a symlink someone put there is never in this list — but re-check the
    // entry type anyway. The list was taken before the deletions above,
    // and "the path I am about to delete is still the thing I looked at"
    // is worth one syscall when the alternative is removing an operator's
    // file.
    if repair && !leaks.is_empty() {
        for l in &leaks {
            match std::fs::symlink_metadata(&l.path) {
                Ok(md) if md.is_file() => {},
                // Gone already, or turned into something else between the
                // scan and now. Either way, not ours to delete.
                _ => {
                    println!("  skipped {} (no longer a regular file)", l.path.display());
                    continue;
                },
            }
            match std::fs::remove_file(&l.path) {
                Ok(()) => println!("  removed {}", l.path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                // Read-only mount, permissions. Surface it: reporting
                // "swept" over a file that is still there sends the
                // operator to free space that is not going to appear.
                Err(e) => bail!("could not remove {}: {e}", l.path.display()),
            }
        }
        let left = leaked_keygen_temp_files(&dir);
        if !left.is_empty() {
            bail!(
                "{} leaked temp file(s) still present after the sweep",
                left.len()
            );
        }
        println!("swept: interrupted-keygen temp files removed");
    }

    Ok(())
}
