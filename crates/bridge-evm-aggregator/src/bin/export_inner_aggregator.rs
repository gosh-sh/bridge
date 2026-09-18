//! Export a bincode-serialized inner [`Snark`] to a production verifier: `<name>.sol` and the `<name>.bin` compiled from it (needs `solc 0.8.19` on `PATH`).
//!
//! ```bash
//! cd crates/bridge-evm-aggregator
//! cargo run --release --bin export-inner-aggregator -- \
//!   --inner-snark /path/to/primary_poseidon.snark \
//!   --out-dir ../../contracts/ethereum/verifiers \
//!   --name PrimaryAggregatorVerifier
//! ```

use std::path::PathBuf;

use bridge_evm_aggregator::{
    aggregator::AggregatorConfig,
    evm_export::aggregate_and_prove_cached,
};
use snark_verifier_sdk::Snark;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let mut inner_path = None;
    let mut out_dir = PathBuf::from("../../contracts/ethereum/verifiers");
    let mut name = None;
    let mut k_outer = None;
    let mut universality = None;
    let mut pk_cache_dir: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--inner-snark" => inner_path = args.next().map(PathBuf::from),
            "--out-dir" => out_dir = args.next().map(PathBuf::from).unwrap_or(out_dir),
            "--name" => name = args.next(),
            "--k-outer" => k_outer = args.next().and_then(|s| s.parse().ok()),
            "--universality" => {
                universality = Some(AggregatorConfig::parse_universality(
                    &args.next().ok_or_else(|| anyhow::anyhow!("--universality needs value"))?,
                )?)
            }
            "--inner-instances" => {
                let _ = args.next();
            }
            // Optional persistent outer PK cache. First run against a new
            // (name, k_outer, lookup_bits, universality, inner-shape) slot
            // does full keygen (~3-5 min at K=21); subsequent runs load PK
            // from disk (~15-60 s). See `aggregator_cache.rs` for slot layout.
            "--pk-cache-dir" => pk_cache_dir = args.next().map(PathBuf::from),
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let inner_path = inner_path.ok_or_else(|| anyhow::anyhow!("--inner-snark required"))?;
    let name = name.ok_or_else(|| anyhow::anyhow!("--name required"))?;

    let inner_bytes = std::fs::read(&inner_path)?;
    let inner_snark: Snark = bincode::deserialize(&inner_bytes)?;
    let config = AggregatorConfig::for_verifier_name_with_overrides(&name, k_outer, universality);
    let export = aggregate_and_prove_cached(
        &name,
        inner_snark,
        config,
        Some(&out_dir),
        pk_cache_dir.as_deref(),
    )?;

    let calldata_path = out_dir.join(format!("{name}_calldata.bin"));
    std::fs::write(&calldata_path, &export.evm_calldata)?;

    let bytecode_len = export
        .verifier_bytecode
        .as_ref()
        .map(Vec::len)
        .expect("an --out-dir is always passed, so the verifier was compiled");
    println!(
        "OK: {} -> {}/{}.{{sol,bin}} ({} B bytecode, {} B source, {} instances, K_outer={}, universality={:?}) + _calldata.bin ({} B)",
        inner_path.display(),
        out_dir.display(),
        name,
        bytecode_len,
        export.verifier_source.len(),
        export.total_instances,
        export.k_outer,
        config.universality,
        export.evm_calldata.len(),
    );
    Ok(())
}
