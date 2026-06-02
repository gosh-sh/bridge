//! Downsize a KZG SRS to a smaller `k`, preserving the ceremony (same tau).
//!
//! The AN-side `ZKHALO2VERIFYWITHVK` opcode rebuilds its verifier params from
//! the chain-wide trusted setup embedded as `KZG_{G0,G2,S_G2}_BYTES` in
//! `tvm_vm::executor::zk_halo2_utils`. Those points come from the chain's
//! `kzg_bn254_19.srs` ceremony — NOT the Hermez/Polygon ceremony the
//! deposit-prover defaulted to. A SHPLONK proof only verifies under the
//! opcode if the prover used the *same* ceremony, so we downsize the chain's
//! k=19 SRS to the deposit circuit's k=18 (tau-preserving: `g2` and `s_g2`
//! are untouched, the G1 powers are truncated and `g_lagrange` recomputed).

use std::fs;

use clap::Parser;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::Bn256,
    poly::{commitment::Params, kzg::commitment::ParamsKZG},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: String,
    #[arg(long)]
    output: String,
    #[arg(long)]
    k: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    println!("Reading SRS {} ...", args.input);
    let mut f = fs::File::open(&args.input)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    println!("  loaded k={}", params.k());
    if params.k() < args.k {
        anyhow::bail!(
            "cannot upsize: input k={} < target k={}",
            params.k(),
            args.k
        );
    }
    if params.k() > args.k {
        println!(
            "Downsizing {} -> {} (tau preserved) ...",
            params.k(),
            args.k
        );
        params.downsize(args.k);
    }
    let mut out = fs::File::create(&args.output)?;
    params.write(&mut out)?;
    println!("Wrote downsized SRS (k={}) -> {}", params.k(), args.output);
    Ok(())
}
