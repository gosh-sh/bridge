//! Export a bincode-serialized inner [`Snark`] to a production Yul `.bin` verifier.
//!
//! ```bash
//! cd crates/bridge-evm-aggregator
//! cargo run --release --bin export-inner-aggregator -- \
//!   --inner-snark /path/to/primary_poseidon.snark \
//!   --out-dir ../../contracts/ethereum/verifiers \
//!   --name PrimaryAggregatorVerifier \
//!   --inner-instances 4
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
    let mut inner_instances = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--inner-snark" => inner_path = args.next().map(PathBuf::from),
            "--out-dir" => out_dir = args.next().map(PathBuf::from).unwrap_or(out_dir),
            "--name" => name = args.next(),
            "--inner-instances" => {
                inner_instances = args.next().and_then(|s| s.parse().ok())
            }
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let inner_path = inner_path.ok_or_else(|| anyhow::anyhow!("--inner-snark required"))?;
    let name = name.ok_or_else(|| anyhow::anyhow!("--name required"))?;
    let num_inner: usize = inner_instances
        .ok_or_else(|| anyhow::anyhow!("--inner-instances required (inner PI count)"))?;

    let inner_bytes = std::fs::read(&inner_path)?;
    let inner_snark: Snark = bincode::deserialize(&inner_bytes)?;
    let config = AggregatorConfig::for_inner_instances(num_inner);
    let export = export_aggregated_snark(&out_dir, &name, inner_snark, config)?;

    println!(
        "OK: {} -> {}/{}.bin ({} B, {} instances, K_outer={})",
        inner_path.display(),
        out_dir.display(),
        name,
        export.verifier_size,
        export.total_instances,
        export.k_outer,
    );
    Ok(())
}
