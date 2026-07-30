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
//! regenerates the Yul `.bin` for the supplied inner snark and asserts it is
//! **byte-identical** to the committed verifier bytecode in
//! `contracts/ethereum/verifiers/<name>.bin`. If the regenerated VK ever drifts
//! (wrong inner shape / config / SRS), the produced calldata would be rejected
//! on-chain — so we refuse to emit it.
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
use bridge_evm_aggregator::{aggregator::AggregatorConfig, evm_export::aggregate_and_prove};
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
    let mut allow_bin_drift = false;

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
            // Escape hatch for the very first bootstrap of a verifier whose .bin
            // is not committed yet. Never use once a verifier is deployed.
            "--allow-bin-drift" => allow_bin_drift = true,
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
    let export = aggregate_and_prove(&name, inner_snark, config, None)
        .context("aggregate + evm-proof (aggregate_and_prove)")?;

    // Self-check: regenerated Yul bytecode must match the committed/deployed one.
    let committed_bin = verifiers_dir.join(format!("{name}.bin"));
    if committed_bin.exists() {
        let committed = std::fs::read(&committed_bin)?;
        if committed != export.verifier_bytecode {
            let msg = format!(
                "regenerated {name}.bin ({} B) != committed {} ({} B): aggregator VK drift -- the \
                 deployed verifier would REJECT this calldata (check inner-snark shape / \
                 AggregatorConfig / SRS)",
                export.verifier_bytecode.len(),
                committed_bin.display(),
                committed.len(),
            );
            if allow_bin_drift {
                eprintln!("WARNING (--allow-bin-drift): {msg}");
            } else {
                anyhow::bail!(msg);
            }
        } else {
            println!(
                "VK match: regenerated {name}.bin == committed ({} B) [OK]",
                committed.len()
            );
        }
    } else if !allow_bin_drift {
        anyhow::bail!(
            "committed verifier {} not found; pass --allow-bin-drift only for first-time \
             bootstrap of an undeployed verifier",
            committed_bin.display()
        );
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
