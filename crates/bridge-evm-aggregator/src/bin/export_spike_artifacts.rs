//! Export M2 multiply-spike aggregator verifier + EVM calldata for Foundry tests.
//!
//! ```bash
//! cd crates/bridge-evm-aggregator
//! cargo run --release --bin export-spike-artifacts
//! ```

use std::path::PathBuf;

use bridge_evm_aggregator::evm_export::export_multiply_spike;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/ethereum/test/fixtures/r15_spike");

    println!("Exporting spike artefacts to {}", out.display());
    let arts = export_multiply_spike(&out)?;

    println!(
        "OK: verifier={} B, calldata={} B, instances={}, inner {:?}*{:?}={:?}",
        arts.verifier_size,
        arts.evm_calldata.len(),
        arts.agg_instances.len(),
        arts.inner_a,
        arts.inner_b,
        arts.inner_c,
    );
    Ok(())
}
