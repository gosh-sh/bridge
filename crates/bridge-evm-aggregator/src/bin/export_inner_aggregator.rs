//! Export a bincode-serialized inner [`Snark`] to a production Yul `.bin` verifier.
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
    evm_export::export_aggregated_snark,
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
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let inner_path = inner_path.ok_or_else(|| anyhow::anyhow!("--inner-snark required"))?;
    let name = name.ok_or_else(|| anyhow::anyhow!("--name required"))?;

    let inner_bytes = std::fs::read(&inner_path)?;
    let inner_snark: Snark = bincode::deserialize(&inner_bytes)?;
    let config = AggregatorConfig::for_verifier_name_with_overrides(&name, k_outer, universality);
    let export = export_aggregated_snark(&out_dir, &name, inner_snark, config)?;

    println!(
        "OK: {} -> {}/{}.bin ({} B, {} instances, K_outer={}, universality={:?})",
        inner_path.display(),
        out_dir.display(),
        name,
        export.verifier_size,
        export.total_instances,
        export.k_outer,
        config.universality,
    );
    Ok(())
}
