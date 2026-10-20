use std::{fs, path::PathBuf};

use clap::Parser;
use deposit_prover::synthetic::{build_with_deposit_id, SyntheticSpec};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    set_dir: PathBuf,
    #[arg(long, default_value_t = 10)]
    count: u64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    for id in 0..args.count {
        let input = build_with_deposit_id(SyntheticSpec::type2_direct(), id);
        let mut json = serde_json::to_value(input)?;
        json["fixture_kind"] = "synthetic".into();
        let dir = args.set_dir.join(format!("proof_{id:02}"));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("input.json"), serde_json::to_vec_pretty(&json)?)?;
    }
    Ok(())
}
