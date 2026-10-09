//! M7 runtime aggregation: turn a Poseidon inner [`Snark`] into the EVM
//! calldata (`instances ‖ proof`) that the **already-deployed** aggregator Yul
//! verifier accepts.
//!
//! This is the per-proof runtime counterpart of `export-inner-aggregator`
//! (which is the one-time verifier-generation step). The aggregator VK is
//! deterministic in `(SRS, AggregatorConfig, inner-snark shape)` and, under
//! `VerifierUniversality::Full`, independent of the inner snark's *values* — so
//! a fresh proof of the same inner circuit yields fresh calldata that the
//! same on-chain verifier accepts. To also bind the aggregator to the
//! specific inner VK, the aggregator exposes the Poseidon digest of the
//! inner-circuit VK witnesses as the final public instance (slot
//! `12 + NUM_INNER`); the on-chain adapter compares that slot against its
//! immutable `vkDigest` pin, so a same-shape but differently-constrained
//! inner circuit is rejected on chain.
//!
//! To make the *shape* half of the guarantee self-checking rather than
//! assumed, this bin regenerates the verifier's Solidity source for the
//! supplied inner snark and asserts it is **byte-identical** to the
//! committed `contracts/ethereum/verifiers/<name>.sol`. The source is fully
//! determined by the aggregator VK, so a drifted shape (wrong inner shape
//! / config / SRS) fails that byte-equality check and we refuse to emit
//! calldata. What the byte-equality check does **not** catch is a
//! same-shape rotation of the inner circuit — that is exactly what the
//! `vkDigest` on-chain pin covers. Nothing is compiled here, so no `solc`
//! is needed; that the committed `.sol` compiles to the deployed `.bin`
//! is checked where verifiers are regenerated
//! (`scripts/check_verifier_sources.sh`).
//!
//! ```bash
//! cd crates/bridge-evm-aggregator
//! cargo +nightly run --release --bin aggregate-proof -- \
//!   --inner-snark ../../proofs/bound/poseidon-snark/circuit4.snark \
//!   --name BridgeWithdrawalAggregatorVerifier \
//!   --out ../../proofs/bound/calldata/circuit4_calldata.bin
//! ```

use std::path::PathBuf;

use anyhow::Context;
use bridge_evm_aggregator::{
    aggregator::AggregatorConfig,
    evm_export::aggregate_and_prove_cached,
    verifier_source::{check_committed_source, SourceCheck},
};
use snark_verifier_sdk::Snark;

/// Logs go to stderr, at `info` for this tool unless `RUST_LOG` says
/// otherwise: its callers forward stderr into their own logs, and the pk
/// cache's notices — waiting for another keygen, a filesystem without
/// locks, leftovers removed — are for the operator. Into a pipe, no colour
/// and no time stamp: the caller stamps the lines it forwards.
fn init_logging() {
    use std::io::IsTerminal as _;

    let logs = tracing_subscriber::fmt()
        .with_env_filter(bridge_evm_aggregator::logging::filter())
        .with_writer(std::io::stderr);
    if std::io::stderr().is_terminal() {
        logs.init();
    } else {
        logs.with_ansi(false).without_time().init();
    }
}

/// Write `slot` to `path` through a temporary name beside it, so that a
/// reader sees the whole name or none. The proof is done by then: a failure
/// costs the caller only its note of the slot, and is a warning.
fn report_slot(path: &std::path::Path, slot: &str) {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let written = std::fs::write(&tmp, slot).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        std::fs::remove_file(&tmp).ok();
        tracing::warn!(path = %path.display(), error = %e, "could not report the pk-cache slot");
    }
}

fn main() -> anyhow::Result<()> {
    init_logging();

    let mut args = std::env::args().skip(1);
    let mut inner_path: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut verifiers_dir = PathBuf::from("../../contracts/ethereum/verifiers");
    let mut k_outer = None;
    let mut universality = None;
    let mut allow_source_drift = false;
    let mut pk_cache_dir: Option<PathBuf> = None;
    let mut slot_report: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--inner-snark" => inner_path = args.next().map(PathBuf::from),
            "--name" => name = args.next(),
            "--out" => out_path = args.next().map(PathBuf::from),
            "--verifiers-dir" => {
                verifiers_dir = args.next().map(PathBuf::from).unwrap_or(verifiers_dir)
            }
            "--k-outer" => k_outer = args.next().and_then(|s| s.parse().ok()),
            "--universality" => {
                universality = Some(AggregatorConfig::parse_universality(
                    &args.next().ok_or_else(|| anyhow::anyhow!("--universality needs value"))?,
                )?)
            }
            // Escape hatch for the very first bootstrap of a verifier whose
            // .sol is not committed yet. Never use once a verifier is deployed.
            "--allow-source-drift" => allow_source_drift = true,
            // Optional persistent outer PK cache. First run against a new
            // (name, k_outer, lookup_bits, universality, inner-shape) slot
            // does full keygen at K=21 — a cold run, keygen and proof, took
            // ~2 min on the test host; later runs load the key from disk.
            // See `aggregator_cache.rs` for slot layout.
            "--pk-cache-dir" => pk_cache_dir = args.next().map(PathBuf::from),
            // Where to write the stem of the pk-cache slot this run used,
            // once its calldata is written: `ackinacki-bridge withdraw`
            // notes it, so that its next preflight knows the key is there.
            // A path of the caller's own, not one in the shared cache.
            "--slot-report" => slot_report = args.next().map(PathBuf::from),
            // Side-effect-free probe. `ackinacki-bridge` preflight runs
            // this before an irreversible burn to prove the binary is
            // present, executable on this architecture, and is actually
            // this tool rather than something else parked at the path —
            // so the usage text must keep naming the real flags.
            "--help" | "-h" => {
                println!(
                    "aggregate-proof --inner-snark <path> --name <verifier> --out <path>\n\
                     \x20 [--verifiers-dir <dir>] [--k-outer <n>] [--universality <mode>]\n\
                     \x20 [--allow-source-drift] [--pk-cache-dir <dir>] [--slot-report <path>]"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let inner_path = inner_path.ok_or_else(|| anyhow::anyhow!("--inner-snark required"))?;
    let name = name.ok_or_else(|| anyhow::anyhow!("--name required"))?;
    let out_path = out_path.ok_or_else(|| anyhow::anyhow!("--out required"))?;
    tracing::info!(
        verifier = %name,
        inner_snark = %inner_path.display(),
        pk_cache_dir = %pk_cache_dir
            .as_deref()
            .map_or_else(|| "none".to_string(), |d| d.display().to_string()),
        "aggregate-proof started"
    );

    let inner_bytes = std::fs::read(&inner_path)
        .with_context(|| format!("read inner snark {}", inner_path.display()))?;
    let inner_snark: Snark = bincode::deserialize(&inner_bytes)
        .with_context(|| format!("deserialize inner snark {}", inner_path.display()))?;

    let config = AggregatorConfig::for_verifier_name_with_overrides(&name, k_outer, universality);

    // Pure in-memory aggregation: no scratch dir, no .sol/.bin written.
    // `pk_cache_dir` (if set) memoises the outer keygen across runs.
    let export = aggregate_and_prove_cached(
        &name,
        inner_snark,
        config,
        None,
        pk_cache_dir.as_deref(),
    )
    .context("aggregate + evm-proof (aggregate_and_prove_cached)")?;

    // Self-check: the regenerated verifier source must match the committed
    // one *byte-for-byte*. This pins the aggregator's shape (columns,
    // gates, lookup arity, `k_outer`, universality), not the inner-circuit
    // VK — a same-shape inner rotation would pass this check and be caught
    // only by the on-chain `vkDigest` pin.
    match check_committed_source(&verifiers_dir, &name, &export.verifier_source)
        .with_context(|| format!("read committed {name}.sol in {}", verifiers_dir.display()))?
    {
        SourceCheck::Match(len) => {
            println!("Aggregator shape match: regenerated {name}.sol == committed ({len} B) [OK]")
        }
        SourceCheck::Drift(msg) if allow_source_drift => {
            eprintln!("WARNING (--allow-source-drift): {msg}")
        }
        SourceCheck::Drift(msg) => anyhow::bail!(msg),
        SourceCheck::Missing(path) if allow_source_drift => eprintln!(
            "WARNING (--allow-source-drift): no committed {}; self-check skipped",
            path.display()
        ),
        SourceCheck::Missing(path) => anyhow::bail!(
            "committed verifier source {} not found; pass --allow-source-drift only for \
             first-time bootstrap of an undeployed verifier",
            path.display()
        ),
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, &export.evm_calldata)?;
    if let (Some(path), Some(slot)) = (slot_report.as_deref(), export.pk_cache_slot.as_deref()) {
        report_slot(path, slot);
    }

    println!(
        "OK: {} -> {} ({} B calldata, {} instances, K_outer={})",
        inner_path.display(),
        out_path.display(),
        export.evm_calldata.len(),
        export.total_instances,
        export.k_outer,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_is_reported_whole_where_the_caller_asked() {
        let dir = std::env::temp_dir().join(format!("agg_report_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aggregator-slot");
        std::fs::write(&path, "a previous run's slot").unwrap();
        report_slot(&path, "BridgeWithdrawalAggregatorVerifier__v3__0123abcd");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "BridgeWithdrawalAggregatorVerifier__v3__0123abcd"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "and nothing else is left beside it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
