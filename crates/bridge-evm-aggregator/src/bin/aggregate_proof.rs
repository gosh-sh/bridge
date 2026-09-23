//! M7 runtime aggregation: turn a Poseidon inner [`Snark`] into the EVM
//! calldata (`instances ‖ proof`) that the **already-deployed** aggregator Yul
//! verifier accepts.
//!
//! This is the per-proof runtime counterpart of `export-inner-aggregator`
//! (which is the one-time verifier-generation step). The aggregator VK is
//! deterministic in `(SRS, AggregatorConfig, inner-snark shape)` and, under
//! `VerifierUniversality::Full`, independent of the inner snark's *values* — so
//! a fresh inner snark yields fresh calldata that the same on-chain verifier
//! accepts.
//!
//! To make that guarantee *self-checking* rather than assumed, this bin
//! regenerates the verifier's Solidity source for the supplied inner snark and
//! asserts it is **byte-identical** to the committed
//! `contracts/ethereum/verifiers/<name>.sol`. The source is fully determined by
//! the aggregator VK, so a drifted VK (wrong inner shape / config / SRS) —
//! whose calldata the deployed verifier would reject — fails the comparison and
//! we refuse to emit it. Nothing is compiled here, so no `solc` is needed; that
//! the committed `.sol` compiles to the deployed `.bin` is checked where
//! verifiers are regenerated (`scripts/check_verifier_sources.sh`).
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let mut inner_path: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut verifiers_dir = PathBuf::from("../../contracts/ethereum/verifiers");
    let mut k_outer = None;
    let mut universality = None;
    let mut allow_source_drift = false;
    let mut pk_cache_dir: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--inner-snark" => inner_path = args.next().map(PathBuf::from),
            "--name" => name = args.next(),
            "--out" => out_path = args.next().map(PathBuf::from),
            "--verifiers-dir" => {
                verifiers_dir = args.next().map(PathBuf::from).unwrap_or(verifiers_dir)
            },
            "--k-outer" => k_outer = args.next().and_then(|s| s.parse().ok()),
            "--universality" => {
                universality = Some(AggregatorConfig::parse_universality(
                    &args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--universality needs value"))?,
                )?)
            },
            // Escape hatch for the very first bootstrap of a verifier whose
            // .sol is not committed yet. Never use once a verifier is deployed.
            "--allow-source-drift" => allow_source_drift = true,
            // Optional persistent outer PK cache. First run against a new
            // (name, k_outer, lookup_bits, universality, inner-shape) slot
            // does full keygen (~3-5 min at K=21); subsequent runs load PK
            // from disk (~15-60 s). See `aggregator_cache.rs` for slot layout.
            "--pk-cache-dir" => pk_cache_dir = args.next().map(PathBuf::from),
            // Side-effect-free probe. `ackinacki-bridge` preflight runs
            // this before an irreversible burn to prove the binary is
            // present, executable on this architecture, and is actually
            // this tool rather than something else parked at the path —
            // so the usage text must keep naming the real flags.
            "--help" | "-h" => {
                println!(
                    "aggregate-proof --inner-snark <path> --name <verifier> --out <path>\n\x20 \
                     [--verifiers-dir <dir>] [--k-outer <n>] [--universality <mode>]\n\x20 \
                     [--allow-source-drift] [--pk-cache-dir <dir>]"
                );
                return Ok(());
            },
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let inner_path = inner_path.ok_or_else(|| anyhow::anyhow!("--inner-snark required"))?;
    let name = name.ok_or_else(|| anyhow::anyhow!("--name required"))?;
    let out_path = out_path.ok_or_else(|| anyhow::anyhow!("--out required"))?;

    let inner_bytes = std::fs::read(&inner_path)
        .with_context(|| format!("read inner snark {}", inner_path.display()))?;
    let inner_snark: Snark = bincode::deserialize(&inner_bytes)
        .with_context(|| format!("deserialize inner snark {}", inner_path.display()))?;

    let config = AggregatorConfig::for_verifier_name_with_overrides(&name, k_outer, universality);

    // Pure in-memory aggregation: no scratch dir, no .sol/.bin written.
    // `pk_cache_dir` (if set) memoises the outer keygen across runs.
    let export =
        aggregate_and_prove_cached(&name, inner_snark, config, None, pk_cache_dir.as_deref())
            .context("aggregate + evm-proof (aggregate_and_prove_cached)")?;

    // Self-check: the regenerated verifier source must match the committed one.
    match check_committed_source(&verifiers_dir, &name, &export.verifier_source)
        .with_context(|| format!("read committed {name}.sol in {}", verifiers_dir.display()))?
    {
        SourceCheck::Match(len) => {
            println!("VK match: regenerated {name}.sol == committed ({len} B) [OK]")
        },
        SourceCheck::Drift(msg) if allow_source_drift => {
            eprintln!("WARNING (--allow-source-drift): {msg}")
        },
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
